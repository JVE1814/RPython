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

fn tlbc_code(f: &PyObject) -> PyResult<PyRef> {
    let co = if crate::types::code::is_code(f) { f.to_ref() } else { ab::getattr(f, crate::name!("__code__"))? };
    if !crate::types::code::is_code(&co) {
        raise!(TypeError, "expected a function or code object");
    }
    Ok(co)
}

/// The current thread's copy of the (specialised) bytecode of a function.
fn get_tlbc(_m: &PyObject, f: &PyObject) -> PyResult {
    let co = tlbc_code(f)?;
    let _ = crate::types::code::code_data(&co);
    Ok(crate::types::bytes::new_bytes(&crate::interp::adaptive::adaptive_code(&co)))
}

/// An identifier of the current thread's copy of a function's bytecode.
fn get_tlbc_id(_m: &PyObject, f: &PyObject) -> PyResult {
    let co = tlbc_code(f)?;
    let cd = crate::types::code::code_data(&co);
    Ok(int::new_int(crate::interp::adaptive::view_id(cd) as i64))
}

/// (tail3) The path-calculation program (modules/getpath.py), compiled: test_getpath
/// executes it against a mocked namespace when no source checkout is around.
fn get_getpath_codeobject(_m: &PyObject) -> PyResult {
    let src = crate::cold_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/modules/getpath.py"));
    Ok(crate::compile::compile_str(src, "getpath.py", crate::compile::Mode::Exec, 0, -1)?.code)
}

fn compiler_cleandoc(_m: &PyObject, s: &PyObject) -> PyResult {
    if !pystr::is_str(s) {
        raise!(TypeError, "compiler_cleandoc() argument must be str, not {}", crate::types::typeobject::tname(s));
    }
    let cps = pystr::str_data(s).to_vec();
    Ok(pystr::from_codepoints(&rpy_compiler::clean_docstring(&cps)))
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

fn has_split_table(_m: &PyObject, o: &PyObject) -> PyResult {
    // (stability3) An instance dict built from inline values keeps the class's
    // attribute names (CPython's split table) for as long as only string keys are added
    // and it stays within the inline capacity; any other key makes it an ordinary table.
    if !dict::is_dict_exact(o) || o.ob_flags.load(std::sync::atomic::Ordering::Relaxed) & dict::OBF_FROM_INSTANCE == 0 {
        return Ok(bool_::new_bool(false));
    }
    let keys = dict::keys_vec(o);
    let split = keys.len() <= INLINE_VALUES_MAX && keys.iter().all(|k| pystr::is_str_exact(k));
    Ok(bool_::new_bool(split))
}

/// Stop every other thread and restart them (the stop-the-world handshake, gc.rs).
fn test_stop_the_world(_m: &PyObject) -> PyResult {
    crate::gc::test_stop_the_world();
    Ok(none::none_ref())
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

/// (tail2) type_assign_specific_version_unsafe(type, version): force a version tag.
fn type_assign_specific_version_unsafe(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("type_assign_specific_version_unsafe", args, 2, 2)?;
    if !crate::types::typeobject::is_type(args[0]) {
        raise!(TypeError, "type_assign_specific_version_unsafe() argument 1 must be a type");
    }
    let v = ab::as_i64(args[1])? as u32;
    crate::interp::cache::set_type_tag_unchecked(args[0].as_ptr() as *mut PyTypeObject, v);
    Ok(none::none_ref())
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
        // read tp_subclasses directly: `type.__subclasses__` looked up on `type`
        // itself would be the unbound method
        for s in crate::types::typeobject::subclasses(tp) {
            stack.push(s.into_ref());
        }
    }
    Ok(out)
}

/// The dunder names that can appear as slot wrappers in a type dict (names in the
/// slot table that have a wrapper; repeated names are listed once per entry).
fn identify_type_slot_wrappers(_m: &PyObject) -> PyResult {
    let names = crate::types::slots::SLOTDEFS
        .iter()
        .filter(|d| d.wrapper.is_some())
        .map(|d| crate::types::str::new_str(d.name))
        .collect();
    Ok(list::new_list(names))
}

// ---- PEP 523 frame-evaluation hook (test form) and rare-event counters

// (capi2) the hook itself lives in interp/pep523.rs

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
    rare_event(RareEvent::SetEvalFrameFunc);
    Ok(none::none_ref())
}
fn set_eval_frame_interp(_m: &PyObject, l: &PyObject) -> PyResult {
    if !list::is_list(l) {
        raise!(TypeError, "argument must be a list");
    }
    crate::interp::pep523::install(crate::interp::pep523::OUTER, Some(l));
    rare_event(RareEvent::SetEvalFrameFunc);
    Ok(none::none_ref())
}
fn set_eval_frame_default(_m: &PyObject) -> PyResult {
    if crate::interp::pep523::installed() {
        rare_event(RareEvent::SetEvalFrameFunc);
    }
    crate::interp::pep523::install(crate::interp::pep523::NONE, None);
    Ok(none::none_ref())
}

/// Events that invalidate optimistic assumptions of the specializer/JIT, counted as
/// CPython counts them for `_testinternalcapi.get_rare_event_counters()`.
#[derive(Clone, Copy)]
pub enum RareEvent {
    SetClass = 0,
    SetBases = 1,
    SetEvalFrameFunc = 2,
    BuiltinDict = 3,
    FuncModification = 4,
}
const RARE_NAMES: [&str; 5] = ["set_class", "set_bases", "set_eval_frame_func", "builtin_dict", "func_modification"];
static RARE: [std::sync::atomic::AtomicU64; 5] = [const { std::sync::atomic::AtomicU64::new(0) }; 5];

#[inline]
pub fn rare_event(e: RareEvent) {
    RARE[e as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn get_rare_event_counters(_m: &PyObject) -> PyResult {
    let d = dict::new_dict();
    for (i, k) in RARE_NAMES.iter().enumerate() {
        dict::set_item_str(&d, k, &int::new_int(RARE[i].load(std::sync::atomic::Ordering::Relaxed) as i64))?;
    }
    Ok(d)
}
fn reset_rare_event_counters(_m: &PyObject) -> PyResult {
    for c in RARE.iter() {
        c.store(0, std::sync::atomic::Ordering::Relaxed);
    }
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
/// Shared part of EncodeLocaleEx / DecodeLocaleEx (text-or-bytes, current_locale, errors):
/// the filesystem codec with the three error handlers the startup converters know.
/// A conversion failure is reported as RuntimeError("encode error: pos=N, reason=R").
fn locale_convert(args: &[&PyObject], encode: bool) -> PyResult {
    if args.len() != 3 {
        raise!(TypeError, "expected 3 arguments, got {}", args.len());
    }
    let errors = pystr::to_string_lossy(args[2]);
    if !matches!(errors.as_str(), "strict" | "surrogateescape" | "surrogatepass") {
        raise!(ValueError, "unsupported error handler");
    }
    let codec = pystr::new_str("utf-8");
    let meth = if encode { "encode" } else { "decode" };
    match ab::call_method_str(args[0], meth, &[&codec, args[2]]) {
        Ok(v) => Ok(v),
        Err(e) => {
            if !exc_matches(crate::types::exceptions::UnicodeError()) {
                return Err(e);
            }
            let exc = take_exc().expect("exception set");
            let pos = ab::getattr_str(&exc, "start").and_then(|p| ab::str_string(&p)).unwrap_or_default();
            let reason = ab::getattr_str(&exc, "reason").and_then(|r| ab::str_string(&r)).unwrap_or_default();
            clear_exc();
            raise!(RuntimeError, "{} error: pos={}, reason={}", meth, pos, reason);
        }
    }
}

/// _PyTraceMalloc_GetTraceback(domain, ptr): the frames recorded for a traced block, or None.
fn tracemalloc_get_traceback(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    check_args(args, 2)?;
    let domain = ab::as_i64(args[0])?;
    let ptr = int_arg(args[1])?.and_then(|p| usize::try_from(p).ok());
    match (u32::try_from(domain), ptr) {
        (Ok(d), Some(p)) => crate::modules::_tracemalloc::get_traceback(d, p),
        _ => raise!(OverflowError, "domain or pointer out of range"),
    }
}

fn encode_locale_ex(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    locale_convert(args, true)
}

fn decode_locale_ex(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    locale_convert(args, false)
}

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

/// set_update(set, iterable) -> 0: in-place update of a (mutable) set. (capi2)
fn set_update_fn(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 {
        raise!(TypeError, "set_update expected 2 arguments, got {}", args.len());
    }
    if !crate::types::set::is_set(args[0]) {
        return Err(crate::errors::system_error("bad argument to internal function"));
    }
    crate::types::set::set_update(args[0], args[1])?;
    Ok(int::new_int(0))
}

/// set_next_entry(set, pos) -> (1, next_pos, hash, item) or None at the end. (capi2)
fn set_next_entry(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 {
        raise!(TypeError, "set_next_entry expected 2 arguments, got {}", args.len());
    }
    if !crate::types::set::is_anyset(args[0]) {
        return Err(crate::errors::system_error("bad argument to internal function"));
    }
    let pos = ab::as_i64(args[1])?.max(0) as usize;
    let items = crate::types::set::items_with_hash(args[0]);
    match items.get(pos) {
        Some((k, h)) => Ok(tuple::new_tuple(vec![int::new_int(1), int::new_int(pos as i64 + 1), int::new_int(*h as i64), k.clone()])),
        None => Ok(none::none_ref()),
    }
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
            ("set_update", FAST, set_update_fn),
            ("set_next_entry", FAST, set_next_entry),
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
            ("get_object_dict_values", O, get_object_dict_values),
            ("compiler_cleandoc", O, compiler_cleandoc),
            ("get_getpath_codeobject", NOARGS, get_getpath_codeobject),
            ("get_tlbc", O, get_tlbc),
            ("get_tlbc_id", O, get_tlbc_id),
            ("dict_getitem_knownhash", FAST, dict_getitem_knownhash),
            ("get_recursion_depth", NOARGS, get_recursion_depth),
            ("test_stop_the_world", NOARGS, test_stop_the_world),
            ("get_long_lived_total", NOARGS, get_long_lived_total),
            ("pymem_getallocatorsname", NOARGS, pymem_getallocatorsname),
            ("get_c_recursion_remaining", NOARGS, get_c_recursion_remaining),
            ("get_stack_pointer", NOARGS, get_stack_pointer),
            ("type_assign_specific_version_unsafe", FAST, type_assign_specific_version_unsafe),
            ("get_stack_margin", NOARGS, get_stack_margin),
            ("_set_interrupt", NOARGS, set_interrupt),
            ("iframe_getcode", O, iframe_getcode),
            ("iframe_getline", O, iframe_getline),
            ("iframe_getlasti", O, iframe_getlasti),
            ("get_co_framesize", O, get_co_framesize),
            ("is_static_immortal", O, is_static_immortal),
            ("py_thread_id", NOARGS, py_thread_id),
            ("get_static_builtin_types", NOARGS, get_static_builtin_types),
            ("identify_type_slot_wrappers", NOARGS, identify_type_slot_wrappers),
            ("EncodeLocaleEx", FAST, encode_locale_ex),
            ("_PyTraceMalloc_GetTraceback", FAST, tracemalloc_get_traceback),
            ("_PyTime_FromSeconds", O, pytime_from_seconds),
            ("_PyTime_FromSecondsObject", FAST, pytime_from_seconds_object),
            ("_PyTime_AsTimeval", FAST, pytime_as_timeval),
            ("_PyTime_AsMilliseconds", FAST, pytime_as_milliseconds),
            ("_PyTime_AsMicroseconds", FAST, pytime_as_microseconds),
            ("_PyTime_ObjectToTime_t", FAST, object_to_time_t),
            ("_PyTime_ObjectToTimeval", FAST, object_to_timeval),
            ("_PyTime_ObjectToTimespec", FAST, object_to_timespec),
            ("DecodeLocaleEx", FAST, decode_locale_ex),
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
    // (capi3) self-tests of runtime internals and the debug-allocator probes
    crate::builtins::add_methods_to_dict(&d, crate::modules::testinternalcapi_selftest::methods(), Some(&m))?;
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

// ---- timestamp conversion helpers (test_time's TestCPyTime / TestOldPyTime)
//
// Nanosecond timestamps are signed 64-bit integers.  Each helper takes the
// rounding mode the C-level converters use: 0 floor, 1 ceiling, 2 half-even,
// 3 away from zero.

const NS_PER_SEC: i128 = 1_000_000_000;
const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;

#[derive(Clone, Copy)]
enum Rounding {
    Floor,
    Ceiling,
    HalfEven,
    Up,
}

fn rounding_arg(o: &PyObject) -> PyResult<Rounding> {
    Ok(match ab::as_i64(o)? {
        0 => Rounding::Floor,
        1 => Rounding::Ceiling,
        2 => Rounding::HalfEven,
        3 => Rounding::Up,
        _ => raise!(ValueError, "invalid rounding mode"),
    })
}

fn round_float(x: f64, mode: Rounding) -> f64 {
    match mode {
        Rounding::Floor => x.floor(),
        Rounding::Ceiling => x.ceil(),
        Rounding::HalfEven => x.round_ties_even(),
        Rounding::Up => {
            if x >= 0.0 {
                x.ceil()
            } else {
                x.floor()
            }
        }
    }
}

/// n / d rounded with `mode` (d > 0), computed exactly.
fn div_rounded(n: i128, d: i128, mode: Rounding) -> i128 {
    let q = n.div_euclid(d);
    let r = n.rem_euclid(d);
    if r == 0 {
        return q;
    }
    let up = match mode {
        Rounding::Floor => false,
        Rounding::Ceiling => true,
        Rounding::Up => n > 0,
        Rounding::HalfEven => match (2 * r).cmp(&d) {
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Equal => q % 2 != 0,
        },
    };
    if up { q + 1 } else { q }
}

fn pytime_overflow() -> PyErr {
    new_err(crate::types::exceptions::OverflowError(), "timestamp out of range for C PyTime_t")
}

fn time_t_overflow() -> PyErr {
    new_err(crate::types::exceptions::OverflowError(), "timestamp out of range for platform time_t")
}

fn nan_error() -> PyErr {
    value_error("Invalid value NaN (not a number)")
}

/// Exact integer value of an int argument, None when it does not fit i128.
fn int_arg(o: &PyObject) -> PyResult<Option<i128>> {
    let i = ab::index(o)?;
    Ok(int::as_i128(&i))
}

/// A nanosecond timestamp argument (must fit 64 bits).
fn pytime_arg(o: &PyObject) -> PyResult<i64> {
    match int_arg(o)? {
        Some(v) if i64::try_from(v).is_ok() => Ok(v as i64),
        _ => Err(pytime_overflow()),
    }
}

fn float_to_i64(d: f64, overflow: fn() -> PyErr) -> PyResult<i64> {
    if d.is_nan() {
        return Err(nan_error());
    }
    if !(-TWO_POW_63..TWO_POW_63).contains(&d) {
        return Err(overflow());
    }
    Ok(d as i64)
}

fn check_args(args: &[&PyObject], n: usize) -> PyResult<()> {
    if args.len() != n {
        raise!(TypeError, "function takes exactly {} arguments ({} given)", n, args.len());
    }
    Ok(())
}

fn pytime_from_seconds(_m: &PyObject, o: &PyObject) -> PyResult {
    if !int::is_int(o) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", crate::types::typeobject::tname(o));
    }
    let secs = match int_arg(o)? {
        Some(v) if i32::try_from(v).is_ok() => v,
        _ => raise!(OverflowError, "signed integer is greater than maximum"),
    };
    Ok(int::new_int_i128(secs * NS_PER_SEC))
}

fn pytime_from_seconds_object(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    check_args(args, 2)?;
    let mode = rounding_arg(args[1])?;
    let o = args[0];
    if crate::types::float::is_float(o) {
        let d = crate::types::float::float_value(o);
        if d.is_nan() {
            return Err(nan_error());
        }
        let ns = float_to_i64(round_float(d * 1e9, mode), pytime_overflow)?;
        return Ok(int::new_int(ns));
    }
    let ns = int_arg(o)?.and_then(|s| s.checked_mul(NS_PER_SEC)).filter(|v| i64::try_from(*v).is_ok());
    match ns {
        Some(v) => Ok(int::new_int_i128(v)),
        None => Err(pytime_overflow()),
    }
}

fn pytime_as_unit(args: &[&PyObject], ns_per_unit: i128) -> PyResult<i128> {
    check_args(args, 2)?;
    let ns = pytime_arg(args[0])?;
    let mode = rounding_arg(args[1])?;
    Ok(div_rounded(ns as i128, ns_per_unit, mode))
}

fn pytime_as_timeval(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let us = pytime_as_unit(args, 1_000)?;
    let (sec, usec) = (us.div_euclid(1_000_000), us.rem_euclid(1_000_000));
    Ok(tuple::new_tuple(vec![int::new_int_i128(sec), int::new_int_i128(usec)]))
}

fn pytime_as_milliseconds(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    Ok(int::new_int_i128(pytime_as_unit(args, 1_000_000)?))
}

fn pytime_as_microseconds(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    Ok(int::new_int_i128(pytime_as_unit(args, 1_000)?))
}

/// Integer seconds argument as time_t.
fn time_t_from_int(o: &PyObject) -> PyResult<i64> {
    match int_arg(o)? {
        Some(v) if i64::try_from(v).is_ok() => Ok(v as i64),
        _ => Err(time_t_overflow()),
    }
}

fn object_to_time_t(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    check_args(args, 2)?;
    let mode = rounding_arg(args[1])?;
    let o = args[0];
    let secs = if crate::types::float::is_float(o) {
        float_to_i64(round_float(crate::types::float::float_value(o), mode), time_t_overflow)?
    } else {
        time_t_from_int(o)?
    };
    Ok(int::new_int(secs))
}

/// (seconds, fraction) with the fraction in 1/`per_sec` units, always in [0, per_sec).
fn object_to_sec_frac(args: &[&PyObject], per_sec: i64) -> PyResult {
    check_args(args, 2)?;
    let mode = rounding_arg(args[1])?;
    let o = args[0];
    let (secs, frac) = if crate::types::float::is_float(o) {
        let d = crate::types::float::float_value(o);
        if d.is_nan() {
            return Err(nan_error());
        }
        let whole = d.trunc();
        let mut frac = round_float((d - whole) * per_sec as f64, mode);
        let mut whole = whole;
        if frac < 0.0 {
            frac += per_sec as f64;
            whole -= 1.0;
        } else if frac >= per_sec as f64 {
            frac -= per_sec as f64;
            whole += 1.0;
        }
        (float_to_i64(whole, time_t_overflow)?, frac as i64)
    } else {
        (time_t_from_int(o)?, 0)
    };
    Ok(tuple::new_tuple(vec![int::new_int(secs), int::new_int(frac)]))
}

fn object_to_timeval(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    object_to_sec_frac(args, 1_000_000)
}

fn object_to_timespec(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    object_to_sec_frac(args, 1_000_000_000)
}
