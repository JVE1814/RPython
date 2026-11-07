//! `_testinternalcapi`: our own implementation of the internal test helpers that the
//! regression suite uses. CPython builds this module from internal headers that are
//! not part of the spec (D2); only the functions/constants the tests call exist here,
//! with behaviour derived from what the tests observe.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, dict, int, list, none, str as pystr, tuple};

/// An instance keeps "inline values" while its attribute storage is the dict that
/// was created with it (not replaced/deleted) and small enough to fit the
/// per-type inline area.
pub const INLINE_VALUES_MAX: usize = 30;

fn has_inline_values(_m: &PyObject, o: &PyObject) -> PyResult {
    let r = unsafe {
        let tp = o.type_ptr();
        if (*tp).tp_flags & Py_TPFLAGS_INLINE_VALUES == 0 || !crate::alloc::has_managed(o.as_ptr()) {
            false
        } else if o.ob_gc_bits.load(std::sync::atomic::Ordering::Relaxed) & crate::alloc::GC_BIT_DICT_DETACHED != 0 {
            false
        } else if crate::shape::has_inline_values(o) {
            true
        } else {
            // the dict materialised from the inline values (shape.rs)
            let d = *crate::alloc::managed_dict_slot(o.as_ptr());
            d.is_null() || (d as usize & 1 == 0 && dict::dict_len(&*d) <= INLINE_VALUES_MAX)
        }
    };
    Ok(bool_::new_bool(r))
}

/// The instance's attribute values in its class's key order ('<NULL>' for names this
/// instance lacks), as CPython reports the inline values array. A dict materialised
/// from the inline values counts as still sharing them; an assigned dict (or one with
/// names the class never stored inline) has no values array: None.
fn get_object_dict_values(_m: &PyObject, o: &PyObject) -> PyResult {
    let tp = o.type_ptr();
    let order = crate::shape::type_key_order(tp);
    if order.is_empty() || unsafe { !crate::alloc::has_managed(o.as_ptr()) } {
        return Ok(none::none_ref());
    }
    let null = || pystr::new_str("<NULL>");
    let mut out = Vec::with_capacity(order.len());
    if crate::shape::has_inline_values(o) {
        for &k in &order {
            out.push(match crate::shape::get(o, unsafe { &*k }) {
                crate::shape::Get::Found(v) => v,
                _ => null(),
            });
        }
        return Ok(tuple::new_tuple(out));
    }
    if o.ob_gc_bits.load(std::sync::atomic::Ordering::Relaxed) & crate::alloc::GC_BIT_DICT_DETACHED != 0 {
        return Ok(none::none_ref());
    }
    let d = unsafe { *crate::alloc::managed_dict_slot(o.as_ptr()) };
    if d.is_null() || d as usize & 1 != 0 || !dict::is_dict_exact(unsafe { &*d }) {
        return Ok(none::none_ref());
    }
    let d = unsafe { &*d };
    for k in dict::keys_vec(d) {
        if !pystr::is_str_exact(&k) || !order.iter().any(|&n| pystr::str_eq(unsafe { &*n }, &k)) {
            return Ok(none::none_ref());
        }
    }
    for &k in &order {
        out.push(dict::get_item_str(d, unsafe { &*k }).unwrap_or_else(null));
    }
    Ok(tuple::new_tuple(out))
}

/// `_PyDict_GetItem_KnownHash` with the hash supplied by the caller.
fn dict_getitem_knownhash(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    crate::args::check_pos("dict_getitem_knownhash", a, 3, 3)?;
    if !dict::is_dict(a[0]) {
        raise!(SystemError, "bad argument to internal function");
    }
    let _hash = ab::as_i64(a[2])?;
    match dict::get_item(a[0], a[1])? {
        Some(v) => Ok(v),
        None => {
            crate::types::dict_methods::raise_key_error(a[1]);
            Err(PyErr)
        }
    }
}

fn has_split_table(_m: &PyObject, _o: &PyObject) -> PyResult {
    // instance dicts are combined tables in this runtime
    Ok(bool_::new_bool(false))
}

fn get_recursion_depth(_m: &PyObject) -> PyResult {
    Ok(int::new_int(crate::threads::current().recursion_depth.get() as i64))
}

/// Name of the allocator family as a free-threaded build reports it: objects come from
/// per-thread heaps (our small-object allocator plays the part mimalloc plays in
/// CPython), with the debug hooks under PYTHONMALLOC=debug or -X dev.
fn pymem_getallocatorsname(_m: &PyObject) -> PyResult {
    let c = crate::modules::sys_full::config();
    let requested = if c.ignore_environment == 0 { std::env::var("PYTHONMALLOC").ok() } else { None };
    let name = match requested.as_deref() {
        Some("malloc") => "malloc",
        Some("malloc_debug") => "malloc_debug",
        Some("debug") | Some("mimalloc_debug") | Some("pymalloc_debug") => "mimalloc_debug",
        _ if c.dev_mode => "mimalloc_debug",
        _ => "mimalloc",
    };
    Ok(crate::types::str::new_str(name))
}

fn get_long_lived_total(_m: &PyObject) -> PyResult {
    Ok(int::new_int(crate::gc_support::LONG_LIVED_TOTAL.load(std::sync::atomic::Ordering::Relaxed) as i64))
}

/// Estimate of how many more interpreter-loop recursions fit on the native stack.
fn get_c_recursion_remaining(_m: &PyObject) -> PyResult {
    let ts = crate::threads::current();
    let x = 0u8;
    let sp = &x as *const u8 as usize;
    let lim = ts.stack_soft_limit();
    let rem = if sp > lim { (sp - lim) / 1000 } else { 0 };
    Ok(int::new_int(rem as i64))
}

fn get_stack_pointer(_m: &PyObject) -> PyResult {
    let x = 0u8;
    Ok(int::new_int(&x as *const u8 as usize as i64))
}

fn get_stack_margin(_m: &PyObject) -> PyResult {
    let ts = crate::threads::current();
    Ok(int::new_int((ts.stack_soft_limit() - ts.stack_hard_limit()) as i64))
}

fn set_interrupt(_m: &PyObject) -> PyResult {
    crate::modules::signal::set_interrupt(libc::SIGINT)?;
    Ok(none::none_ref())
}

/// `SelfInterruptingContextManager`: a context manager written in C in CPython. Its
/// __enter__ requests a KeyboardInterrupt, which must arrive inside the with block
/// (after the block is protected, so that __exit__ runs).
#[repr(C)]
struct SelfInterrupting {
    ob_base: PyObject,
    within: bool,
}

static mut SICM_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn sicm(o: &PyObject) -> &mut SelfInterrupting {
    unsafe { &mut *(o.as_ptr() as *mut SelfInterrupting) }
}

unsafe extern "C" fn sicm_new(tp: *mut PyTypeObject, _args: *mut PyObject, _kw: *mut PyObject) -> *mut PyObject {
    unsafe { crate::types::typeobject::generic_alloc(tp, 0) }
}

fn sicm_enter(o: &PyObject) -> PyResult {
    sicm(o).within = true;
    crate::modules::signal::set_interrupt(libc::SIGINT)?;
    Ok(o.to_ref())
}

fn sicm_exit(o: &PyObject, _exc: &[&PyObject]) -> PyResult {
    sicm(o).within = false;
    Ok(bool_::new_bool(false))
}

fn sicm_within(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(sicm(o).within))
}

fn sicm_type() -> PyResult<PyRef> {
    unsafe {
        let t = &mut *(&raw mut SICM_TYPE);
        if t.tp_flags & Py_TPFLAGS_READY == 0 {
            t.tp_name = c"_testinternalcapi.SelfInterruptingContextManager".as_ptr();
            t.tp_basicsize = std::mem::size_of::<SelfInterrupting>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT;
            t.tp_new = Some(sicm_new);
            t.tp_alloc = Some(crate::types::typeobject::generic_alloc);
            t.tp_dealloc = Some(crate::alloc::free_object_c);
            t.tp_methods = crate::mdefs![
                ("__enter__", NOARGS, sicm_enter),
                ("__exit__", FAST, sicm_exit),
                ("within", NOARGS, sicm_within),
            ];
            crate::types::typeobject::type_ready(&raw mut SICM_TYPE)?;
        }
        Ok(crate::types::typeobject::type_ref(&raw mut SICM_TYPE))
    }
}

fn frame_arg(o: &PyObject) -> PyResult<&mut crate::interp::frame::Frame> {
    match crate::types::frameobject::frame_of(o) {
        Some(f) => Ok(f),
        None => raise!(TypeError, "argument must be a frame"),
    }
}

fn iframe_getcode(_m: &PyObject, o: &PyObject) -> PyResult {
    let f = frame_arg(o)?;
    Ok(f.code().to_ref())
}
fn iframe_getline(_m: &PyObject, o: &PyObject) -> PyResult {
    let f = frame_arg(o)?;
    let l = crate::types::code::line_for_instr(f.code(), f.lasti as usize).map_or(-1, |l| l as i64);
    Ok(int::new_int(l))
}
fn iframe_getlasti(_m: &PyObject, o: &PyObject) -> PyResult {
    let f = frame_arg(o)?;
    Ok(int::new_int(crate::types::code::lasti_bytes(f.code(), f.lasti as usize)))
}

fn get_co_framesize(_m: &PyObject, o: &PyObject) -> PyResult {
    if !crate::types::code::is_code(o) {
        raise!(TypeError, "argument must be a code object");
    }
    let c = crate::types::code::code(o);
    Ok(int::new_int(c.obj.co_nlocalsplus as i64 + c.obj.co_stacksize as i64))
}

fn is_static_immortal(_m: &PyObject, o: &PyObject) -> PyResult {
    let tp = o.type_ptr();
    let heap = unsafe { (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE != 0 };
    Ok(bool_::new_bool(o.is_immortal() && (!heap || pystr::is_str(o) || int::is_int(o))))
}

fn py_thread_id(_m: &PyObject) -> PyResult {
    Ok(int::new_int(crate::object::thread_id() as i64))
}

fn get_static_builtin_types(_m: &PyObject) -> PyResult {
    let out = list::new_list(vec![]);
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![crate::types::typeobject::type_ref(crate::types::typeobject::object_type())];
    while let Some(t) = stack.pop() {
        if !seen.insert(t.as_ptr() as usize) {
            continue;
        }
        let tp = t.as_ptr() as *mut PyTypeObject;
        if unsafe { (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE != 0 } {
            continue;
        }
        list::append(&out, t.clone());
        let subs = ab::call_method_str(&t, "__subclasses__", &[])?;
        for s in list::list_items(&subs) {
            stack.push(s.to_ref());
        }
    }
    Ok(out)
}

fn is_specialization_enabled(_m: &PyObject) -> PyResult {
    Ok(bool_::new_bool(false))
}

fn get_rare_event_counters(_m: &PyObject) -> PyResult {
    let d = dict::new_dict();
    for k in ["set_class", "set_bases", "set_eval_frame_func", "builtin_dict", "func_modification"] {
        dict::set_item_str(&d, k, &int::new_int(0))?;
    }
    Ok(d)
}
fn reset_rare_event_counters(_m: &PyObject) -> PyResult {
    Ok(none::none_ref())
}

// (interp1) get_interp_settings is provided by subinterp::testapi
const SRC: &str = "";

/// Runs a queued Python callable (the queue owns one reference to it).
unsafe extern "C" fn run_pending_callable(arg: *mut std::ffi::c_void) -> std::ffi::c_int {
    unsafe {
        let f = PyRef::from_raw(arg as *mut PyObject);
        match ab::call(&f, &[]) {
            Ok(_) => 0,
            Err(_) => -1,
        }
    }
}

/// pending_threadfunc(callback, num=1, *, blocking=False, ensure_added=False): queue
/// `callback` `num` times as a pending call that any thread may run; returns how many
/// were queued (the queue holds at most 300).
fn pending_threadfunc(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("pending_threadfunc", &["callback", "num", "blocking", "ensure_added"], 0, 1);
    let [cb, num, _blocking, ensure] = crate::args::parse::<4>(&P, args, kw)?;
    let cb = cb.unwrap();
    let num = match num {
        Some(n) => ab::as_i64(n)?.max(0) as u64,
        None => 1,
    };
    let ensure = match ensure {
        Some(e) => ab::is_true(e)?,
        None => false,
    };
    let mut added = 0u64;
    while added < num {
        let p = cb.to_ref().into_raw() as *mut std::ffi::c_void;
        if crate::capi::pystate::add_pending_call(run_pending_callable, p, false) < 0 {
            unsafe { decref(p as *mut PyObject) };
            if ensure {
                std::thread::yield_now();
                continue;
            }
            break;
        }
        added += 1;
    }
    Ok(int::new_int(added as i64))
}

pub fn init() -> PyResult {
    let m = crate::types::module::new_module("_testinternalcapi")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("has_inline_values", O, has_inline_values),
            ("pending_threadfunc", FASTKW, pending_threadfunc),
            ("has_split_table", O, has_split_table),
            ("get_object_dict_values", O, get_object_dict_values),
            ("dict_getitem_knownhash", FAST, dict_getitem_knownhash),
            ("get_recursion_depth", NOARGS, get_recursion_depth),
            ("get_long_lived_total", NOARGS, get_long_lived_total),
            ("pymem_getallocatorsname", NOARGS, pymem_getallocatorsname),
            ("get_c_recursion_remaining", NOARGS, get_c_recursion_remaining),
            ("get_stack_pointer", NOARGS, get_stack_pointer),
            ("get_stack_margin", NOARGS, get_stack_margin),
            ("_set_interrupt", NOARGS, set_interrupt),
            ("iframe_getcode", O, iframe_getcode),
            ("iframe_getline", O, iframe_getline),
            ("iframe_getlasti", O, iframe_getlasti),
            ("get_co_framesize", O, get_co_framesize),
            ("is_static_immortal", O, is_static_immortal),
            ("py_thread_id", NOARGS, py_thread_id),
            ("get_static_builtin_types", NOARGS, get_static_builtin_types),
            ("is_specialization_enabled", NOARGS, is_specialization_enabled),
            ("get_rare_event_counters", NOARGS, get_rare_event_counters),
            ("reset_rare_event_counters", NOARGS, reset_rare_event_counters),
            ("get_co_localskinds", O, crate::types::code_introspect::get_co_localskinds),
            ("get_code_var_counts", FASTKW, crate::types::code_introspect::get_code_var_counts),
            ("verify_stateless_code", FASTKW, crate::types::code_introspect::verify_stateless_code),
            ("code_returns_only_none", O, crate::types::code_introspect::code_returns_only_none),
            ("set_vectorcall_nop", O, crate::types::code_introspect::set_vectorcall_nop),
            ("get_next_dict_keys_version", NOARGS, crate::types::code_introspect::get_next_dict_keys_version),
        ],
        Some(&m),
    )?;
    for (k, v) in [
        ("SIZEOF_PYGC_HEAD", 0i64),
        ("SIZEOF_MANAGED_PRE_HEADER", crate::alloc::MANAGED as i64),
        ("SIZEOF_PYOBJECT", std::mem::size_of::<PyObject>() as i64),
        ("SIZEOF_TIME_T", std::mem::size_of::<libc::time_t>() as i64),
        // CPython: an object reaching this many attributes leaves the shared-keys (inline)
        // layout; ours holds INLINE_VALUES_MAX inline, so one more forces the dict
        ("SHARED_KEYS_MAX_SIZE", INLINE_VALUES_MAX as i64 + 1),
        ("SPECIALIZATION_THRESHOLD", 2),
        ("SPECIALIZATION_COOLDOWN", 53),
        ("TIER2_THRESHOLD", 4096),
        ("TIER2_RESUME_THRESHOLD", 4096),
        ("_PY_NSMALLPOSINTS", 1025),
        ("_PY_NSMALLNEGINTS", 5),
        ("_Py_MAX_UNICODE", 0x10ffff),
    ] {
        dict::set_item_str(&d, k, &int::new_int(v))?;
    }
    dict::set_item_str(&d, "__builtins__", &crate::builtins::builtins_module())?;
    crate::modules::compiler_hooks::install(&d, &m)?;
    // (interp1) interpreter helpers (create/exec/destroy, ids, configs, XI data)
    crate::subinterp::testapi::install(&d, &m)?;
    let sicm_tp = sicm_type()?;
    dict::set_item_str(&d, "SelfInterruptingContextManager", &sicm_tp)?;
    let code = crate::compile::compile_str(SRC, "<_testinternalcapi>", crate::compile::Mode::Exec, 0, -1)?;
    crate::interp::eval_code(&code.code, &d, Some(&d))?;
    let _ = tuple::new_tuple(vec![]);
    Ok(m)
}
