//! `_queue`: the unbounded, reentrant SimpleQueue and the Empty exception.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, dict, int, none, str as pystr, tuple};
use std::collections::VecDeque;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicPtr, Ordering::*};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[repr(C)]
struct SimpleQueue {
    ob_base: PyObject,
    weakreflist: *mut PyObject,
    q: Mutex<VecDeque<usize>>,
    cv: Condvar,
}

static mut SQ_TYPE: PyTypeObject = PyTypeObject::ZERO;
static EMPTY: crate::subinterp::InterpPtr = crate::subinterp::InterpPtr::new(); // (interp1) per interpreter

fn sq(o: &PyObject) -> &SimpleQueue {
    unsafe { &*(o.as_ptr() as *const SimpleQueue) }
}

unsafe extern "C" fn sq_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if tp == &raw mut SQ_TYPE && (tuple::tuple_len(&*args) != 0 || (!kw.is_null() && dict::dict_len(&*kw) != 0)) {
            return into_c(Err(err!(TypeError, "SimpleQueue() takes no arguments")));
        }
        let op = ((*tp).tp_alloc.unwrap())(tp, 0);
        if op.is_null() {
            return op;
        }
        let s = op as *mut SimpleQueue;
        std::ptr::write(&raw mut (*s).q, Mutex::new(VecDeque::new()));
        std::ptr::write(&raw mut (*s).cv, Condvar::new());
        op
    }
}
unsafe extern "C" fn sq_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        crate::types::weakref::clear_weakrefs(op);
        let s = op as *mut SimpleQueue;
        let items: Vec<usize> = (*s).q.lock().unwrap().drain(..).collect();
        for p in items {
            decref(p as *mut PyObject);
        }
        std::ptr::drop_in_place(&raw mut (*s).q);
        std::ptr::drop_in_place(&raw mut (*s).cv);
        crate::types::typeobject::generic_free_and_type_decref(op, (*op).type_ptr());
    }
}
unsafe extern "C" fn sq_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let s = &*(op as *const SimpleQueue);
        if let Ok(g) = s.q.try_lock() {
            for &p in g.iter() {
                let r = visit.unwrap()(p as *mut PyObject, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        if crate::types::typeobject::is_heap_type((*op).type_ptr()) {
            return visit.unwrap()((*op).type_ptr() as *mut PyObject, arg);
        }
        0
    }
}
unsafe extern "C" fn sq_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let s = &*(op as *const SimpleQueue);
        let items: Vec<usize> = s.q.lock().unwrap().drain(..).collect();
        for p in items {
            decref(p as *mut PyObject);
        }
        0
    }
}

fn sq_put_impl(o: &PyObject, item: &PyObject) {
    let s = sq(o);
    s.q.lock().unwrap().push_back(item.to_ref().into_raw() as usize);
    s.cv.notify_one();
}
fn sq_put(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("put", &["item", "block", "timeout"], 0, 1);
    let [item, _b, _t] = crate::args::parse::<3>(&P, args, kw)?;
    sq_put_impl(o, item.unwrap());
    Ok(none::none_ref())
}
fn sq_put_nowait(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("put_nowait", &["item"], 0, 1);
    let [item] = crate::args::parse::<1>(&P, args, kw)?;
    sq_put_impl(o, item.unwrap());
    Ok(none::none_ref())
}
fn empty_err() -> PyErr {
    let e = EMPTY.load(Acquire);
    match ab::call(unsafe { &*e }, &[]) {
        Ok(x) => set_exc(x),
        Err(_) => {}
    }
    PyErr
}
fn sq_get_impl(o: &PyObject, block: bool, timeout: Option<Duration>) -> PyResult {
    let s = sq(o);
    if let Some(p) = s.q.lock().unwrap().pop_front() {
        return Ok(unsafe { PyRef::from_raw(p as *mut PyObject) });
    }
    if !block {
        return Err(empty_err());
    }
    let deadline = timeout.map(|t| Instant::now() + t);
    let main = crate::threads::is_main_thread();
    loop {
        let mut wait = match deadline {
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    return Err(empty_err());
                }
                d - now
            }
            None => Duration::from_secs(3600),
        };
        if main {
            crate::modules::signal::check_signals()?;
            wait = wait.min(Duration::from_millis(20));
        }
        let got = crate::threads::blocking(|| {
            let mut g = s.q.lock().unwrap();
            if g.is_empty() {
                g = s.cv.wait_timeout(g, wait).unwrap().0;
            }
            g.pop_front()
        });
        if let Some(p) = got {
            return Ok(unsafe { PyRef::from_raw(p as *mut PyObject) });
        }
    }
}
fn sq_get(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("get", &["block", "timeout"], 0, 0);
    let [b, t] = crate::args::parse::<2>(&P, args, kw)?;
    let block = match b {
        Some(b) => ab::is_true(b)?,
        None => true,
    };
    let timeout = match t {
        // a non-blocking get ignores the timeout
        Some(t) if block && !t.is(none::none()) => {
            let v = ab::float_value(t)?;
            if v < 0.0 {
                raise!(ValueError, "'timeout' must be a non-negative number");
            }
            Some(Duration::from_secs_f64(v.min(crate::modules::_thread::TIMEOUT_MAX)))
        }
        _ => None,
    };
    sq_get_impl(o, block, timeout)
}
fn sq_get_nowait(o: &PyObject) -> PyResult {
    sq_get_impl(o, false, None)
}
fn sq_empty(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(sq(o).q.lock().unwrap().is_empty()))
}
fn sq_qsize(o: &PyObject) -> PyResult {
    Ok(int::new_int(sq(o).q.lock().unwrap().len() as i64))
}
fn sq_class_getitem(cls: &PyObject, item: &PyObject) -> PyResult {
    crate::types::genericalias::new_generic_alias(cls, item)
}

pub fn init_module() -> PyResult {
    unsafe {
        if SQ_TYPE.tp_flags & Py_TPFLAGS_READY == 0 {
            let t = &mut *(&raw mut SQ_TYPE);
            t.tp_name = c"_queue.SimpleQueue".as_ptr();
            t.tp_basicsize = std::mem::size_of::<SimpleQueue>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC;
            t.tp_weaklistoffset = std::mem::offset_of!(SimpleQueue, weakreflist) as isize;
            t.tp_new = Some(sq_new);
            t.tp_alloc = Some(crate::types::typeobject::generic_alloc);
            t.tp_dealloc = Some(sq_dealloc);
            t.tp_traverse = Some(sq_traverse);
            t.tp_clear = Some(sq_clear);
            t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
            t.tp_doc = c"SimpleQueue()\n--\n\nAn unbounded FIFO queue that is safe to use from several threads and\nfrom reentrant code.".as_ptr();
            t.tp_methods = crate::mdefs![
                ("put", FASTKW, sq_put, "put($self, /, item, block=True, timeout=None)\n--\n\nAppend item to the queue; block and timeout are accepted for\ncompatibility with Queue.put() and ignored."),
                ("put_nowait", FASTKW, sq_put_nowait, "put_nowait($self, /, item)\n--\n\nAppend item to the queue without blocking."),
                ("get", FASTKW, sq_get, "get($self, /, block=True, timeout=None)\n--\n\nRemove and return the oldest item, waiting up to timeout seconds\n(forever if None) when block is true; raise Empty otherwise."),
                ("get_nowait", NOARGS, sq_get_nowait),
                ("empty", NOARGS, sq_empty),
                ("qsize", NOARGS, sq_qsize),
                ("__class_getitem__", O, sq_class_getitem; METH_CLASS),
            ];
            crate::types::typeobject::type_ready(&raw mut SQ_TYPE)?;
        }
        if EMPTY.load(Acquire).is_null() {
            let ns = dict::new_dict();
            dict::set_item_str(&ns, "__module__", &pystr::new_str("_queue"))?;
            dict::set_item_str(&ns, "__doc__", &pystr::new_str("Exception raised by Queue.get(block=0)/get_nowait()."))?;
            let bases = tuple::new_tuple(vec![crate::types::typeobject::type_ref(crate::types::exceptions::Exception())]);
            let cls = ab::call(
                crate::types::typeobject::type_obj(crate::types::typeobject::type_type()),
                &[&pystr::new_str("Empty"), &bases, &ns],
            )?;
            EMPTY.store(cls.into_raw(), Release);
        }
    }
    let m = crate::types::module::new_module("_queue")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "SimpleQueue", crate::types::typeobject::type_obj(&raw mut SQ_TYPE))?;
    dict::set_item_str(&d, "Empty", unsafe { &*EMPTY.load(Acquire) })?;
    Ok(m)
}
