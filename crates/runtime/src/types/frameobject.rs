//! Frame objects (`sys._getframe()`, tracebacks). Wraps an interpreter `Frame`;
//! once the frame finishes, the frame object takes ownership of it.

use crate::errors::*;
use crate::ffi::*;
use crate::interp::frame::{Frame, FrameState};
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, int, none, str as pystr};
use crate::raise;
use rpy_compiler::bytecode as bc;
use std::ffi::{c_int, c_void};

/// The first fields keep the offsets of CPython's frame object, because compiled
/// extensions write some of them directly: Cython stores the traceback line of the
/// frames it creates with PyFrame_New into `f_lineno` (offset 56).
#[repr(C)]
pub struct PyFrameObject {
    pub ob_base: PyObject,
    /// Offset of CPython's `f_back` (not used here: see the `f_back` getter).
    pub _f_back: *mut PyObject,
    pub frame: *mut Frame,
    /// Offset of CPython's `f_trace` (the trace function lives in `Frame`).
    pub _f_trace: *mut PyObject,
    /// Line number stored by C code; 0 means "compute it from the instruction".
    pub f_lineno: c_int,
    pub _f_trace_lines: u8,
    pub _f_trace_opcodes: u8,
    /// Owned frame once the execution finished.
    pub owned: *mut Frame,
    pub weakreflist: *mut PyObject,
    pub locals_snapshot: *mut PyObject,
}

const _: () = assert!(std::mem::offset_of!(PyFrameObject, f_lineno) == 56);

#[unsafe(no_mangle)]
pub static mut PyFrame_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn frame_type() -> *mut PyTypeObject {
    &raw mut PyFrame_Type
}
pub fn is_frame(o: &PyObject) -> bool {
    o.is_type(frame_type())
}
fn fo(o: &PyObject) -> &mut PyFrameObject {
    unsafe { &mut *(o.as_ptr() as *mut PyFrameObject) }
}
pub fn frame_of(o: &PyObject) -> Option<&mut Frame> {
    let p = fo(o).frame;
    if p.is_null() { None } else { Some(unsafe { &mut *p }) }
}

pub fn new_frame_object(f: &mut Frame) -> *mut PyObject {
    unsafe {
        let op = crate::alloc::alloc_object(frame_type(), std::mem::size_of::<PyFrameObject>());
        (*(op as *mut PyFrameObject)).frame = f as *mut Frame;
        crate::gc::track(op);
        // The interpreter frame holds a "lazy" reference: the frame object stays alive
        // while referenced elsewhere; the frame keeps a borrowed back-pointer and the
        // object keeps one extra self-reference until the frame finishes.
        op
    }
}

/// Transfer ownership of a finished frame to its frame object.
pub unsafe fn take_ownership(fobj: *mut PyObject, frame: Box<Frame>) {
    unsafe {
        let f = &mut *(fobj as *mut PyFrameObject);
        let p = Box::into_raw(frame);
        f.owned = p;
        f.frame = p;
        // drop the self-reference held on behalf of the running frame
        decref(fobj);
    }
}

unsafe extern "C" fn frame_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let f = &mut *(op as *mut PyFrameObject);
        crate::types::weakref::clear_weakrefs_at(op, &mut f.weakreflist);
        if !f.owned.is_null() {
            let mut b = Box::from_raw(f.owned);
            f.owned = std::ptr::null_mut();
            f.frame = std::ptr::null_mut();
            b.frame_obj = std::ptr::null_mut();
            drop(b);
        } else if !f.frame.is_null() {
            (*f.frame).frame_obj = std::ptr::null_mut();
        }
        xdecref(f.locals_snapshot);
        crate::alloc::free_object(op);
    }
}

unsafe extern "C" fn frame_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let f = &*(op as *mut PyFrameObject);
        if f.owned.is_null() {
            return 0;
        }
        let fr = &*f.owned;
        let v = visit.unwrap();
        for i in 0..fr.stack_top {
            let p = fr.slots[i];
            if !p.is_null() {
                let r = v(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        if !f.locals_snapshot.is_null() {
            let r = v(f.locals_snapshot, arg);
            if r != 0 {
                return r;
            }
        }
        for p in [fr.func, fr.globals, fr.locals, fr.f_trace] {
            if !p.is_null() {
                let r = v(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

unsafe extern "C" fn frame_clear_slot(op: *mut PyObject) -> c_int {
    unsafe {
        let f = &mut *(op as *mut PyFrameObject);
        if !f.owned.is_null() {
            (*f.owned).clear_locals();
        }
        0
    }
}

unsafe extern "C" fn frame_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let s = match frame_of(o) {
            Some(f) => {
                let cd = crate::types::code::code(f.code());
                format!(
                    "<frame at {:p}, file '{}', line {}, code {}>",
                    op,
                    pystr::to_string_lossy(&cd.d.filename),
                    f.lineno(),
                    pystr::to_string_lossy(&cd.d.co_name)
                )
            }
            None => format!("<frame at {:p}>", op),
        };
        pystr::new_str(&s).into_raw()
    }
}

fn need(o: &PyObject) -> PyResult<&mut Frame> {
    match frame_of(o) {
        Some(f) => Ok(f),
        None => raise!(RuntimeError, "frame has been cleared"),
    }
}
fn f_code(o: &PyObject) -> PyResult {
    Ok(need(o)?.code().to_ref())
}
fn f_globals(o: &PyObject) -> PyResult {
    Ok(need(o)?.globals().to_ref())
}
fn f_builtins(o: &PyObject) -> PyResult {
    Ok(need(o)?.builtins().to_ref())
}
fn f_lineno(o: &PyObject) -> PyResult {
    let f = need(o)?;
    let stored = fo(o).f_lineno;
    if stored > 0 {
        return Ok(int::new_int(stored as i64));
    }
    let l = if f.state == FrameState::Created { crate::types::code::code(f.code()).obj.co_firstlineno } else { f.lineno() };
    Ok(if l < 0 { none::none_ref() } else { int::new_int(l as i64) })
}
fn f_set_lineno(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let f = need(o)?;
    crate::interp::jump::set_lineno(f, v)
}
fn f_lasti(o: &PyObject) -> PyResult {
    let f = need(o)?;
    Ok(int::new_int(crate::types::code::lasti_offset(f.code(), f.lasti as i64)))
}
fn f_back(o: &PyObject) -> PyResult {
    let f = need(o)?;
    if f.prev.is_null() {
        // a suspended/finished frame has no back frame; a running one might be the top
        return Ok(none::none_ref());
    }
    let p = crate::interp::eval::frame_object(unsafe { &mut *f.prev });
    unsafe { incref(p) };
    Ok(unsafe { PyRef::from_raw(p) })
}
fn f_trace(o: &PyObject) -> PyResult {
    let f = need(o)?;
    Ok(if f.f_trace.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(f.f_trace) } })
}
fn f_set_trace(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let f = need(o)?;
    let nv = match v {
        Some(v) if !v.is(none::none()) => v.to_ref().into_raw(),
        _ => std::ptr::null_mut(),
    };
    let old = std::mem::replace(&mut f.f_trace, nv);
    unsafe { xdecref(old) };
    Ok(())
}
fn f_trace_lines(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(need(o)?.f_trace_lines))
}
fn f_set_trace_lines(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) if bool_::is_bool(v) => {
            need(o)?.f_trace_lines = v.is(bool_::true_obj());
            Ok(())
        }
        _ => raise!(TypeError, "attribute value type must be bool"),
    }
}
fn f_trace_opcodes(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(need(o)?.f_trace_opcodes))
}
fn f_set_trace_opcodes(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) if bool_::is_bool(v) => {
            need(o)?.f_trace_opcodes = v.is(bool_::true_obj());
            Ok(())
        }
        _ => raise!(TypeError, "attribute value type must be bool"),
    }
}

/// Snapshot of a frame's locals as a dict (fast locals + cells; or the locals mapping).
pub fn locals_dict(f: &Frame) -> PyResult {
    if !f.locals.is_null() {
        return Ok(unsafe { (*f.locals).to_ref() });
    }
    // jit: unboxed locals of tier-2 code
    crate::interp::jit::opt::sync_frame(f as *const Frame as *mut Frame);
    let d = dict::new_dict();
    let cd = crate::types::code::code_data(f.code());
    for i in 0..f.nlocalsplus {
        let kind = cd.localspluskinds[i];
        if kind & bc::fast::HIDDEN != 0 {
            continue;
        }
        let p = f.slots[i];
        if p.is_null() {
            continue;
        }
        let v = if kind & (bc::fast::CELL | bc::fast::FREE) != 0 {
            let pr = unsafe { &*p };
            if pr.is_type(crate::types::funcobject::cell_type()) {
                match crate::types::funcobject::cell_get(pr) {
                    Some(v) => v,
                    None => continue,
                }
            } else {
                pr.to_ref()
            }
        } else {
            unsafe { PyRef::from_borrowed(p) }
        };
        dict::set_item(&d, &cd.localsplusnames[i], &v)?;
    }
    if let Some(e) = crate::types::frameobject_proxy::extra_locals_of(f.frame_obj) {
        dict::merge_dict(&d, &e, true)?;
    }
    Ok(d)
}

fn f_locals(o: &PyObject) -> PyResult {
    let f = need(o)?;
    if !f.locals.is_null() {
        return Ok(unsafe { (*f.locals).to_ref() });
    }
    crate::types::frameobject_proxy::new_proxy(o)
}
fn f_generator(o: &PyObject) -> PyResult {
    let f = need(o)?;
    Ok(if f.generator.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(f.generator) } })
}
fn frame_clear_method(o: &PyObject) -> PyResult {
    let f = need(o)?;
    match f.state {
        FrameState::Executing => raise!(RuntimeError, "cannot clear an executing frame"),
        FrameState::Suspended => {
            if !f.generator.is_null() {
                let g = unsafe { (*f.generator).to_ref() };
                crate::types::genobject::gen_close(&g)?;
            } else {
                raise!(RuntimeError, "cannot clear a suspended frame");
            }
        }
        _ => {}
    }
    if let Some(f) = frame_of(o) {
        f.clear_locals();
    }
    Ok(none::none_ref())
}
fn frame_sizeof(_o: &PyObject) -> PyResult {
    Ok(int::new_int(std::mem::size_of::<PyFrameObject>() as i64))
}

pub fn init_types() {
    unsafe {
        let t = &mut *frame_type();
        t.tp_name = c"frame".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyFrameObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(frame_dealloc);
        t.tp_traverse = Some(frame_traverse);
        t.tp_clear = Some(frame_clear_slot);
        t.tp_repr = Some(frame_repr);
        t.tp_weaklistoffset = std::mem::offset_of!(PyFrameObject, weakreflist) as isize;
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_setattro = Some(object_generic_setattro);
        t.tp_getset = crate::getset_table![
            ("f_code", f_code),
            ("f_globals", f_globals),
            ("f_builtins", f_builtins),
            ("f_locals", f_locals),
            ("f_lineno", f_lineno, f_set_lineno),
            ("f_lasti", f_lasti),
            ("f_back", f_back),
            ("f_trace", f_trace, f_set_trace),
            ("f_trace_lines", f_trace_lines, f_set_trace_lines),
            ("f_trace_opcodes", f_trace_opcodes, f_set_trace_opcodes),
            ("f_generator", f_generator),
        ];
        t.tp_methods = crate::mdefs![("clear", NOARGS, frame_clear_method), ("__sizeof__", NOARGS, frame_sizeof)];
    }
}
