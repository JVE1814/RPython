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

const SRC: &str = r#"
class SelfInterruptingContextManager:
    """__enter__ requests a KeyboardInterrupt that is delivered at the next
    eval-breaker check, i.e. inside the with block."""
    def __init__(self):
        self._within = False
    def __enter__(self):
        self._within = True
        _set_interrupt()
        return self
    def __exit__(self, *exc):
        self._within = False
        return False
    def within(self):
        return self._within

def get_interp_settings(interp=None):
    return 0
"#;

pub fn init() -> PyResult {
    let m = crate::types::module::new_module("_testinternalcapi")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("has_inline_values", O, has_inline_values),
            ("has_split_table", O, has_split_table),
            ("get_recursion_depth", NOARGS, get_recursion_depth),
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
        ("SHARED_KEYS_MAX_SIZE", INLINE_VALUES_MAX as i64),
        ("SPECIALIZATION_THRESHOLD", 2),
        ("SPECIALIZATION_COOLDOWN", 53),
        ("TIER2_THRESHOLD", 4096),
        ("TIER2_RESUME_THRESHOLD", 4096),
        ("_PY_NSMALLPOSINTS", 1025),
        ("_PY_NSMALLNEGINTS", 5),
    ] {
        dict::set_item_str(&d, k, &int::new_int(v))?;
    }
    dict::set_item_str(&d, "__builtins__", &crate::builtins::builtins_module())?;
    let code = crate::compile::compile_str(SRC, "<_testinternalcapi>", crate::compile::Mode::Exec, 0, -1)?;
    crate::interp::eval_code(&code.code, &d, Some(&d))?;
    let _ = tuple::new_tuple(vec![]);
    Ok(m)
}
