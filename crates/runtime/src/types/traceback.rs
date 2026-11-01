//! Traceback objects (layout of the public `PyTracebackObject`).

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{int, none};
use crate::raise;
use std::ffi::{c_int, c_void};

#[repr(C)]
pub struct PyTracebackObject {
    pub ob_base: PyObject,
    pub tb_next: *mut PyObject,
    pub tb_frame: *mut PyObject,
    /// byte offset of the instruction (4 bytes per instruction)
    pub tb_lasti: c_int,
    pub tb_lineno: c_int,
}

#[unsafe(no_mangle)]
pub static mut PyTraceBack_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn traceback_type() -> *mut PyTypeObject {
    &raw mut PyTraceBack_Type
}
pub fn is_traceback(o: &PyObject) -> bool {
    o.is_type(traceback_type())
}
fn tb(o: &PyObject) -> &mut PyTracebackObject {
    unsafe { &mut *(o.as_ptr() as *mut PyTracebackObject) }
}

/// New traceback entry for `frame` in front of `next`.
pub fn new_traceback(next: Option<&PyObject>, frame: &PyObject, lasti_instr: i32, lineno: i32) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(traceback_type(), std::mem::size_of::<PyTracebackObject>());
        let t = &mut *(op as *mut PyTracebackObject);
        t.tb_next = next.map_or(std::ptr::null_mut(), |n| n.to_ref().into_raw());
        t.tb_frame = frame.to_ref().into_raw();
        t.tb_lasti = lasti_instr * 4;
        t.tb_lineno = lineno;
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}

unsafe extern "C" fn tb_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let t = &mut *(op as *mut PyTracebackObject);
        xdecref(t.tb_next);
        xdecref(t.tb_frame);
        crate::alloc::free_object(op);
    }
}
unsafe extern "C" fn tb_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let t = &*(op as *mut PyTracebackObject);
        for p in [t.tb_next, t.tb_frame] {
            if !p.is_null() {
                let r = visit.unwrap()(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}
unsafe extern "C" fn tb_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let t = &mut *(op as *mut PyTracebackObject);
        let a = std::mem::replace(&mut t.tb_next, std::ptr::null_mut());
        let b = std::mem::replace(&mut t.tb_frame, std::ptr::null_mut());
        xdecref(a);
        xdecref(b);
        0
    }
}
fn get_next(o: &PyObject) -> PyResult {
    let n = tb(o).tb_next;
    Ok(if n.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(n) } })
}
fn set_next(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        None => raise!(TypeError, "can't delete tb_next attribute"),
        Some(v) => v,
    };
    let newv = if v.is(none::none()) {
        std::ptr::null_mut()
    } else if is_traceback(v) {
        // loop check
        let mut cur = v.as_ptr();
        while !cur.is_null() {
            if cur == o.as_ptr() {
                raise!(ValueError, "traceback loop detected");
            }
            cur = unsafe { (*(cur as *mut PyTracebackObject)).tb_next };
        }
        v.to_ref().into_raw()
    } else {
        raise!(TypeError, "expected traceback object or None, got '{}'", tname(v));
    };
    let old = std::mem::replace(&mut tb(o).tb_next, newv);
    unsafe { xdecref(old) };
    Ok(())
}
fn get_frame(o: &PyObject) -> PyResult {
    Ok(unsafe { PyRef::from_borrowed(tb(o).tb_frame) })
}
fn get_lasti(o: &PyObject) -> PyResult {
    Ok(int::new_int(tb(o).tb_lasti as i64))
}
fn get_lineno(o: &PyObject) -> PyResult {
    let l = tb(o).tb_lineno;
    Ok(if l < 0 { none::none_ref() } else { int::new_int(l as i64) })
}
unsafe extern "C" fn tb_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        static P: crate::args::Params = crate::args::Params::new("traceback", &["tb_next", "tb_frame", "tb_lasti", "tb_lineno"], 0, 4);
        let r = (|| -> PyResult {
            let (all, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = all.iter().map(|r| &**r).collect();
            let [next, frame, lasti, lineno] = crate::args::parse::<4>(&P, &refs, kwn.as_deref())?;
            let (next, frame) = (next.unwrap(), frame.unwrap());
            let next = if next.is(none::none()) {
                None
            } else if is_traceback(next) {
                Some(next)
            } else {
                raise!(TypeError, "expected traceback object or None, got '{}'", tname(next));
            };
            if !crate::types::frameobject::is_frame(frame) {
                raise!(TypeError, "TracebackType() argument 'tb_frame' must be frame, not {}", tname(frame));
            }
            let li = crate::abstract_::as_i64(lasti.unwrap())? as i32;
            let ln = crate::abstract_::as_i64(lineno.unwrap())? as i32;
            let t = new_traceback(next, frame, 0, ln);
            (*(t.as_ptr() as *mut PyTracebackObject)).tb_lasti = li;
            Ok(t)
        })();
        into_c(r)
    }
}
fn tb_dir(_o: &PyObject) -> PyResult {
    let names = ["tb_frame", "tb_next", "tb_lasti", "tb_lineno"];
    Ok(crate::types::list::new_list(names.iter().map(|n| crate::types::str::new_str(n)).collect()))
}

pub fn init_types() {
    unsafe {
        let t = &mut *traceback_type();
        t.tp_name = c"traceback".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyTracebackObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(tb_dealloc);
        t.tp_traverse = Some(tb_traverse);
        t.tp_clear = Some(tb_clear);
        t.tp_new = Some(tb_new);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_setattro = Some(object_generic_setattro);
        t.tp_getset = crate::getset_table![
            ("tb_next", get_next, set_next),
            ("tb_frame", get_frame),
            ("tb_lasti", get_lasti),
            ("tb_lineno", get_lineno),
        ];
        t.tp_methods = crate::mdefs![("__dir__", NOARGS, tb_dir)];
    }
}
