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
    Ok(bool_::new_bool(crate::interp::pep523::specialization_enabled()))
}

/// set_eval_frame_record(list) / set_eval_frame_interp(list) / set_eval_frame_default():
/// PEP 523 frame-evaluation hooks recording code names (interp/pep523.rs). (capi2)
fn set_eval_frame_record(_m: &PyObject, l: &PyObject) -> PyResult {
    if !list::is_list(l) {
        raise!(TypeError, "argument must be a list");
    }
    crate::interp::pep523::install(crate::interp::pep523::ALL, Some(l));
    Ok(none::none_ref())
}
fn set_eval_frame_interp(_m: &PyObject, l: &PyObject) -> PyResult {
    if !list::is_list(l) {
        raise!(TypeError, "argument must be a list");
    }
    crate::interp::pep523::install(crate::interp::pep523::OUTER, Some(l));
    Ok(none::none_ref())
}
fn set_eval_frame_default(_m: &PyObject) -> PyResult {
    crate::interp::pep523::install(crate::interp::pep523::NONE, None);
    Ok(none::none_ref())
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

/// tuple_from_pair(a, b) / tuple_from_pair_steal(a, b): a 2-tuple built like the
/// internal pair constructors build it (tracked only if an item may be in a
/// cycle). (hang1)
fn tuple_from_pair(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 {
        raise!(TypeError, "tuple_from_pair expected 2 arguments, got {}", args.len());
    }
    Ok(tuple::new_tuple_from_refs(&[args[0], args[1]]))
}

/// module_get_gc_hooks(module): addresses of the module state's traverse, clear and free
/// functions (0 when absent). (capi2)
fn module_get_gc_hooks(_m: &PyObject, o: &PyObject) -> PyResult {
    if !crate::types::module::is_module(o) {
        raise!(TypeError, "expected a module, got {}", crate::types::typeobject::tname(o));
    }
    let def = crate::types::module::m(o).md_def;
    let (t, c, f) = if def.is_null() {
        (0, 0, 0)
    } else {
        unsafe { ((*def).m_traverse.map_or(0, |p| p as usize), (*def).m_clear.map_or(0, |p| p as usize), (*def).m_free.map_or(0, |p| p as usize)) }
    };
    Ok(tuple::new_tuple(vec![int::new_int_usize(t), int::new_int_usize(c), int::new_int_usize(f)]))
}

/// get_configs(): {"global_config": {legacy global variable: value}}. (capi2)
fn get_configs(_m: &PyObject) -> PyResult {
    let g = dict::new_dict();
    for (k, v) in crate::capi::config::legacy_globals() {
        dict::set_item_str(&g, k, &int::new_int(v as i64))?;
    }
    let out = dict::new_dict();
    dict::set_item_str(&out, "global_config", &g)?;
    Ok(out)
}

/// _pyerr_setkeyerror(arg): raise KeyError(arg) (one argument, never unpacked), replacing a
/// pending exception. (capi2)
fn pyerr_setkeyerror(_m: &PyObject, arg: &PyObject) -> PyResult {
    let _ = memory_error();
    clear_exc();
    let e = ab::call(crate::types::typeobject::type_obj(crate::types::exceptions::KeyError()), &[arg])?;
    set_exc(e);
    Err(PyErr)
}

// ---- mixed complex/real arithmetic helpers (_py_cr_* / _py_rc_*). (capi2)
// The real operand only combines with the real part (or scales both parts), so signed
// zeros and infinities of the other component survive unchanged.

fn cplx_arg(o: &PyObject) -> PyResult<(f64, f64)> {
    let c = unsafe { crate::capi::long::PyComplex_AsCComplex(o.as_ptr()) };
    if c.real == -1.0 && err_occurred() {
        return Err(PyErr);
    }
    Ok((c.real, c.imag))
}

fn real_arg(o: &PyObject) -> PyResult<f64> {
    let v = unsafe { crate::capi::long::PyFloat_AsDouble(o.as_ptr()) };
    if v == -1.0 && err_occurred() {
        return Err(PyErr);
    }
    Ok(v)
}

fn cplx_result(re: f64, im: f64, errno: i64) -> PyResult {
    Ok(tuple::new_tuple(vec![crate::types::complex::new_complex(re, im), int::new_int(errno)]))
}

fn two_args<'a>(args: &'a [&'a PyObject], name: &str) -> PyResult<(&'a PyObject, &'a PyObject)> {
    if args.len() != 2 {
        raise!(TypeError, "{} expected 2 arguments, got {}", name, args.len());
    }
    Ok((args[0], args[1]))
}

fn py_cr_sum(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let (a, b) = two_args(args, "_py_cr_sum")?;
    let ((re, im), r) = (cplx_arg(a)?, real_arg(b)?);
    cplx_result(re + r, im, 0)
}

fn py_cr_diff(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let (a, b) = two_args(args, "_py_cr_diff")?;
    let ((re, im), r) = (cplx_arg(a)?, real_arg(b)?);
    cplx_result(re - r, im, 0)
}

fn py_rc_diff(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let (a, b) = two_args(args, "_py_rc_diff")?;
    let (r, (re, im)) = (real_arg(a)?, cplx_arg(b)?);
    cplx_result(r - re, -im, 0)
}

fn py_cr_prod(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let (a, b) = two_args(args, "_py_cr_prod")?;
    let ((re, im), r) = (cplx_arg(a)?, real_arg(b)?);
    cplx_result(re * r, im * r, 0)
}

fn py_cr_quot(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let (a, b) = two_args(args, "_py_cr_quot")?;
    let ((re, im), r) = (cplx_arg(a)?, real_arg(b)?);
    if r == 0.0 {
        return cplx_result(0.0, 0.0, libc::EDOM as i64);
    }
    cplx_result(re / r, im / r, 0)
}

fn py_rc_quot(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let (a, b) = two_args(args, "_py_rc_quot")?;
    let (x, (c, d)) = (real_arg(a)?, cplx_arg(b)?);
    // x / (c + di), scaled by the larger divisor component to avoid overflow
    if c.abs() >= d.abs() {
        if c == 0.0 && d == 0.0 {
            return cplx_result(0.0, 0.0, libc::EDOM as i64);
        }
        let t = d / c;
        let den = c + d * t;
        cplx_result(x / den, -x * t / den, 0)
    } else if d.abs() >= c.abs() {
        let t = c / d;
        let den = c * t + d;
        cplx_result(x * t / den, -x / den, 0)
    } else if x.is_finite() && (c.is_infinite() || d.is_infinite()) {
        // a finite value over an infinite divisor is zero (the NaN part only lends its sign)
        cplx_result(0.0f64.copysign(x) * 1.0f64.copysign(c), 0.0f64.copysign(-x) * 1.0f64.copysign(d), 0)
    } else {
        cplx_result(f64::NAN, f64::NAN, 0)
    }
}

/// gh_119213_getargs(spam=None): keyword parsing from a subinterpreter. (capi2)
fn gh_119213_getargs(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("gh_119213_getargs", &["spam"], 0, 0);
    let [spam] = crate::args::parse::<1>(&P, args, kw)?;
    Ok(spam.map(|s| s.to_ref()).unwrap_or_else(none::none_ref))
}

/// _PyUnicode_TransformDecimalAndSpaceToASCII(s): decimal digits become ASCII digits,
/// whitespace a space; the first other non-ASCII character becomes '?' and ends the
/// result. (capi2)
fn transform_decimal_and_space(_m: &PyObject, s: &PyObject) -> PyResult {
    if !pystr::is_str(s) {
        return Err(crate::errors::system_error("bad argument to internal function"));
    }
    let mut out = String::new();
    for c in pystr::str_data(s).iter() {
        if rpy_unicode::is_space(c) {
            out.push(' ');
        } else if let Some(d) = rpy_unicode::to_decimal(c) {
            out.push(char::from(b'0' + d as u8));
        } else if c < 128 {
            out.push(char::from(c as u8));
        } else {
            out.push('?');
            break;
        }
    }
    Ok(pystr::new_str(&out))
}

/// normalize_path(path): lexical normalisation of a path (dot segments, repeated
/// separators; a leading "//" but not "///" is kept). (capi2)
fn normalize_path(_m: &PyObject, p: &PyObject) -> PyResult {
    let s: Vec<u32> = if pystr::is_str(p) { pystr::str_data(p).iter().collect() } else { raise!(TypeError, "normalize_path() argument must be str") };
    let slash = '/' as u32;
    let lead = if s.len() >= 2 && s[0] == slash && s[1] == slash && s.get(2) != Some(&slash) { 2 } else if s.first() == Some(&slash) { 1 } else { 0 };
    let mut parts: Vec<&[u32]> = Vec::new();
    for comp in s[lead..].split(|&c| c == slash) {
        if comp.is_empty() || comp == ['.' as u32] {
            continue;
        }
        if comp == ['.' as u32, '.' as u32] {
            if parts.last().is_some_and(|l| *l != ['.' as u32, '.' as u32]) {
                parts.pop();
                continue;
            }
            if lead > 0 {
                continue;
            }
        }
        parts.push(comp);
    }
    let mut out: Vec<u32> = vec![slash; lead];
    for (i, comp) in parts.iter().enumerate() {
        if i > 0 {
            out.push(slash);
        }
        out.extend_from_slice(comp);
    }
    if out.is_empty() {
        out.push('.' as u32);
    }
    Ok(pystr::from_codepoints(&out))
}

pub fn init() -> PyResult {
    let m = crate::types::module::new_module("_testinternalcapi")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("has_inline_values", O, has_inline_values),
            ("module_get_gc_hooks", O, module_get_gc_hooks),
            ("get_configs", NOARGS, get_configs),
            ("_pyerr_setkeyerror", O, pyerr_setkeyerror),
            ("normalize_path", O, normalize_path),
            ("set_eval_frame_record", O, set_eval_frame_record),
            ("set_eval_frame_interp", O, set_eval_frame_interp),
            ("set_eval_frame_default", NOARGS, set_eval_frame_default),
            ("_PyUnicode_TransformDecimalAndSpaceToASCII", O, transform_decimal_and_space),
            ("gh_119213_getargs", FASTKW, gh_119213_getargs),
            ("_py_cr_sum", FAST, py_cr_sum),
            ("_py_cr_diff", FAST, py_cr_diff),
            ("_py_rc_diff", FAST, py_rc_diff),
            ("_py_cr_prod", FAST, py_cr_prod),
            ("_py_cr_quot", FAST, py_cr_quot),
            ("_py_rc_quot", FAST, py_rc_quot),
            ("pending_threadfunc", FASTKW, pending_threadfunc),
            ("tuple_from_pair", FAST, tuple_from_pair),
            ("tuple_from_pair_steal", FAST, tuple_from_pair),
            ("has_split_table", O, has_split_table),
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
