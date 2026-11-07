//! `_interpqueues`: process-wide FIFO queues that any interpreter can put
//! cross-interpreter data into and take it out of.  Blocking and timeouts are
//! done by the Python layer (`concurrent.interpreters._queues`).

use super::xidata::{self, Mode, Xi};
use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::{bool_, int, list, none, str as pystr, tuple};
use crate::{mdefs, raise};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicPtr, Ordering::*};
use std::sync::Mutex;

/// What happens to an item when the interpreter that put it is destroyed.
pub const UNBOUND_REMOVE: i32 = 1;
pub const UNBOUND_ERROR: i32 = 2;
pub const UNBOUND_REPLACE: i32 = 3;

struct Item {
    data: Option<Xi>,
    interp: i64,
    unboundop: i32,
    /// the interpreter is gone (data dropped)
    unbound: bool,
}

struct Queue {
    id: i64,
    items: VecDeque<Item>,
    maxsize: i64,
    unboundop: i32,
    fallback: i32,
    refs: i64,
}

struct Registry {
    queues: Vec<Queue>,
    next_id: i64,
}

static REG: Mutex<Registry> = Mutex::new(Registry { queues: Vec::new(), next_id: 1 });

fn reg() -> std::sync::MutexGuard<'static, Registry> {
    REG.lock().unwrap_or_else(|e| e.into_inner())
}

// --------------------------------------------------------------------- exception types

static QUEUE_ERROR: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());
static QUEUE_NOT_FOUND: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

fn ensure_types() {
    if !QUEUE_NOT_FOUND.load(Acquire).is_null() {
        return;
    }
    static LOCK: Mutex<()> = Mutex::new(());
    let _g = LOCK.lock().unwrap();
    if !QUEUE_NOT_FOUND.load(Acquire).is_null() {
        return;
    }
    super::with_interp(super::main(), || {
        let saved = take_exc();
        let e = crate::modules::devtools::new_exception("concurrent.interpreters._queues", "QueueError", crate::types::exceptions::RuntimeError()).expect("QueueError");
        let _ = ab::setattr(&e, &pystr::new_str("__doc__"), &pystr::new_str("Indicates that a queue-related error happened."));
        let ep = e.into_raw() as *mut PyTypeObject;
        let nf = crate::modules::devtools::new_exception("concurrent.interpreters._queues", "QueueNotFoundError", ep).expect("QueueNotFoundError");
        clear_exc();
        QUEUE_ERROR.store(ep, Release);
        QUEUE_NOT_FOUND.store(nf.into_raw() as *mut PyTypeObject, Release);
        if let Some(e) = saved {
            set_exc(e);
        }
    });
}

fn queue_error(msg: &str) -> PyErr {
    ensure_types();
    new_err(QUEUE_ERROR.load(Acquire), msg)
}

fn not_found(id: i64) -> PyErr {
    ensure_types();
    new_err(QUEUE_NOT_FOUND.load(Acquire), &format!("queue {} not found", id))
}

// --------------------------------------------------------------------- per-interpreter types

/// The Python classes `concurrent.interpreters._queues` registered in this
/// interpreter: (Queue, QueueEmpty, QueueFull).
struct Types(Mutex<Option<(PyRef, PyRef, PyRef)>>);
static TYPES: super::PerInterp<Types> = super::PerInterp::new(|| Types(Mutex::new(None)));

fn registered() -> Option<(PyRef, PyRef, PyRef)> {
    TYPES.get().0.lock().unwrap().clone()
}

fn registered_or_import() -> PyResult<(PyRef, PyRef, PyRef)> {
    if let Some(t) = registered() {
        return Ok(t);
    }
    crate::import::import_absolute("concurrent.interpreters._queues")?;
    match registered() {
        Some(t) => Ok(t),
        None => Err(queue_error("queue types not registered")),
    }
}

/// The id of `o` if it is a queue object of the current interpreter.
pub fn queue_id_of(o: &PyObject) -> PyResult<Option<i64>> {
    let Some((qt, _, _)) = registered() else { return Ok(None) };
    if !crate::types::typeobject::is_subtype(o.type_ptr(), qt.as_ptr() as *mut PyTypeObject) {
        return Ok(None);
    }
    let id = ab::getattr_str(o, "id")?;
    match int::as_i64(&id) {
        Some(v) => Ok(Some(v)),
        None => raise!(TypeError, "bad queue id"),
    }
}

/// A queue object (of the current interpreter) for queue `id`.
pub fn queue_object(id: i64) -> PyResult {
    let (qt, _, _) = registered_or_import()?;
    ab::call(&qt, &[&int::new_int(id)])
}

// --------------------------------------------------------------------- operations

fn id_arg(o: &PyObject) -> PyResult<i64> {
    if pystr::is_str(o) || crate::types::float::is_float(o) {
        raise!(TypeError, "queue ID must be an int, got {}", crate::types::typeobject::tname(o));
    }
    let i = ab::index(o)?;
    match int::as_i64(&i) {
        Some(v) if v >= 0 => Ok(v),
        Some(v) => raise!(ValueError, "queue ID must be a non-negative int, got {}", v),
        None => raise!(OverflowError, "queue ID too large"),
    }
}

fn with_queue<R>(id: i64, f: impl FnOnce(&mut Queue) -> R) -> PyResult<R> {
    let mut g = reg();
    match g.queues.iter_mut().find(|q| q.id == id) {
        Some(q) => Ok(f(q)),
        None => {
            drop(g);
            Err(not_found(id))
        }
    }
}

fn int_opt(o: Option<&PyObject>, default: i64) -> PyResult<i64> {
    match o {
        Some(v) if !v.is(none::none()) => {
            if !int::is_int(v) {
                raise!(TypeError, "expected an int, got {}", crate::types::typeobject::tname(v));
            }
            Ok(int::as_i64(v).unwrap_or(default))
        }
        _ => Ok(default),
    }
}

fn check_unboundop(op: i64, allow_default: bool) -> PyResult<i32> {
    match op {
        -1 if allow_default => Ok(-1),
        1..=3 => Ok(op as i32),
        _ => raise!(ValueError, "unsupported unboundop {}", op),
    }
}

fn q_create(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("create", &["maxsize", "unboundop", "fallback"], 0, 1);
    let [maxsize, unboundop, fallback] = args::parse::<3>(&P, args, kw)?;
    let maxsize = maxsize.unwrap();
    if !int::is_int(maxsize) {
        raise!(TypeError, "maxsize must be an int, got {}", crate::types::typeobject::tname(maxsize));
    }
    let maxsize = int::as_i64(maxsize).unwrap_or(i64::MAX);
    let mut op = check_unboundop(int_opt(unboundop, -1)?, true)?;
    if op == -1 {
        op = UNBOUND_REPLACE;
    }
    let fallback = int_opt(fallback, -1)? as i32;
    let mut g = reg();
    let id = g.next_id;
    g.next_id += 1;
    g.queues.push(Queue { id, items: VecDeque::new(), maxsize, unboundop: op, fallback, refs: 0 });
    Ok(int::new_int(id))
}

fn q_destroy(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("destroy", &["qid"], 0, 1);
    let [qid] = args::parse::<1>(&P, args, kw)?;
    let id = id_arg(qid.unwrap())?;
    let removed = {
        let mut g = reg();
        match g.queues.iter().position(|q| q.id == id) {
            Some(i) => Some(g.queues.remove(i)),
            None => None,
        }
    };
    match removed {
        // the items are dropped outside the lock (they may release other queues)
        Some(q) => {
            drop(q);
            Ok(none::none_ref())
        }
        None => Err(not_found(id)),
    }
}

fn q_list_all(_m: &PyObject) -> PyResult {
    let v: Vec<(i64, i32, i32)> = reg().queues.iter().map(|q| (q.id, q.unboundop, q.fallback)).collect();
    Ok(list::new_list(v.into_iter().map(|(a, b, c)| tuple::new_tuple(vec![int::new_int(a), int::new_int(b as i64), int::new_int(c as i64)])).collect()))
}

fn q_put(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("put", &["qid", "obj", "unboundop", "fallback"], 0, 2);
    let [qid, obj, unboundop, _fallback] = args::parse::<4>(&P, args, kw)?;
    let id = id_arg(qid.unwrap())?;
    let op = check_unboundop(int_opt(unboundop, -1)?, true)?;
    // fail early on a missing queue
    with_queue(id, |_| ())?;
    let data = xidata::get(obj.unwrap(), Mode::Fallback)?;
    let interp = super::current().id;
    let full = with_queue(id, move |q| {
        if q.maxsize > 0 && q.items.len() as i64 >= q.maxsize {
            return Some(data);
        }
        let op = if op == -1 { q.unboundop } else { op };
        q.items.push_back(Item { data: Some(data), interp, unboundop: op, unbound: false });
        None
    })?;
    if let Some(d) = full {
        drop(d);
        let (_, _, fullerr) = registered_or_import()?;
        set_exc(ab::call(&fullerr, &[&pystr::new_str("too many items in queue")])?);
        return Err(PyErr);
    }
    Ok(none::none_ref())
}

fn q_get(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("get", &["qid"], 0, 1);
    let [qid] = args::parse::<1>(&P, args, kw)?;
    let id = id_arg(qid.unwrap())?;
    let item = with_queue(id, |q| q.items.pop_front())?;
    let Some(item) = item else {
        let (_, emptyerr, _) = registered_or_import()?;
        set_exc(ab::call(&emptyerr, &[&pystr::new_str("queue is empty")])?);
        return Err(PyErr);
    };
    if item.unbound {
        return Ok(tuple::new_tuple(vec![none::none_ref(), int::new_int(item.unboundop as i64)]));
    }
    let o = item.data.as_ref().unwrap().to_object()?;
    Ok(tuple::new_tuple(vec![o, none::none_ref()]))
}

fn q_bind(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("bind", &["qid"], 0, 1);
    let [qid] = args::parse::<1>(&P, args, kw)?;
    let id = id_arg(qid.unwrap())?;
    with_queue(id, |q| q.refs += 1)?;
    Ok(none::none_ref())
}

/// Drop one binding; the queue goes away with the last one.
fn release_id(id: i64) -> PyResult<()> {
    let removed = {
        let mut g = reg();
        let Some(pos) = g.queues.iter().position(|q| q.id == id) else {
            drop(g);
            return Err(not_found(id));
        };
        if g.queues[pos].refs <= 0 {
            drop(g);
            return Err(queue_error(&format!("queue {} never bound", id)));
        }
        g.queues[pos].refs -= 1;
        if g.queues[pos].refs == 0 { Some(g.queues.remove(pos)) } else { None }
    };
    drop(removed);
    Ok(())
}

fn q_release(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("release", &["qid"], 0, 1);
    let [qid] = args::parse::<1>(&P, args, kw)?;
    release_id(id_arg(qid.unwrap())?)?;
    Ok(none::none_ref())
}

fn q_get_maxsize(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("get_maxsize", &["qid"], 0, 1);
    let [qid] = args::parse::<1>(&P, args, kw)?;
    let id = id_arg(qid.unwrap())?;
    Ok(int::new_int(with_queue(id, |q| q.maxsize)?))
}

fn q_get_queue_defaults(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("get_queue_defaults", &["qid"], 0, 1);
    let [qid] = args::parse::<1>(&P, args, kw)?;
    let id = id_arg(qid.unwrap())?;
    let (u, f) = with_queue(id, |q| (q.unboundop, q.fallback))?;
    Ok(tuple::new_tuple(vec![int::new_int(u as i64), int::new_int(f as i64)]))
}

fn q_is_full(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("is_full", &["qid"], 0, 1);
    let [qid] = args::parse::<1>(&P, args, kw)?;
    let id = id_arg(qid.unwrap())?;
    let full = with_queue(id, |q| q.maxsize > 0 && q.items.len() as i64 >= q.maxsize)?;
    Ok(bool_::new_bool(full))
}

fn q_get_count(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("get_count", &["qid"], 0, 1);
    let [qid] = args::parse::<1>(&P, args, kw)?;
    let id = id_arg(qid.unwrap())?;
    Ok(int::new_int(with_queue(id, |q| q.items.len() as i64)?))
}

fn q_register_heap_types(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("_register_heap_types", &["queuetype", "emptyerror", "fullerror"], 0, 3);
    let [qt, e, f] = args::parse::<3>(&P, args, kw)?;
    for t in [qt, e, f] {
        if !crate::types::typeobject::is_type(t.unwrap()) {
            raise!(TypeError, "expected a type, got {}", crate::types::typeobject::tname(t.unwrap()));
        }
    }
    *TYPES.get().0.lock().unwrap() = Some((qt.unwrap().to_ref(), e.unwrap().to_ref(), f.unwrap().to_ref()));
    Ok(none::none_ref())
}

/// An interpreter is gone: its items become unbound (or are removed).
pub fn interp_destroyed(interp: i64) {
    let mut dropped: Vec<Xi> = Vec::new();
    {
        let mut g = reg();
        for q in g.queues.iter_mut() {
            let mut kept = VecDeque::with_capacity(q.items.len());
            for mut it in q.items.drain(..) {
                if it.interp != interp || it.unbound {
                    kept.push_back(it);
                    continue;
                }
                if let Some(d) = it.data.take() {
                    dropped.push(d);
                }
                if it.unboundop != UNBOUND_REMOVE {
                    it.unbound = true;
                    kept.push_back(it);
                }
            }
            q.items = kept;
        }
    }
    drop(dropped);
}

pub fn init_module() -> PyResult {
    ensure_types();
    let methods = mdefs![
        ("create", FASTKW, q_create, "create(maxsize, unboundop, fallback) -> qid\n\nCreate a new cross-interpreter queue and return its unique generated ID."),
        ("destroy", FASTKW, q_destroy, "destroy(qid)\n\nClear and destroy the queue."),
        ("list_all", NOARGS, q_list_all, "list_all() -> [(qid, unboundop, fallback)]\n\nReturn the list of IDs for all queues."),
        ("put", FASTKW, q_put, "put(qid, obj, unboundop=-1, fallback=-1)\n\nAdd the object's data to the queue."),
        ("get", FASTKW, q_get, "get(qid) -> (obj, unboundop)\n\nReturn a new object from the data at the front of the queue."),
        ("bind", FASTKW, q_bind, "bind(qid)\n\nTake a reference to the identified queue."),
        ("release", FASTKW, q_release, "release(qid)\n\nRelease a reference to the queue."),
        ("get_maxsize", FASTKW, q_get_maxsize, "get_maxsize(qid)\n\nReturn the maximum number of items in the queue."),
        ("get_queue_defaults", FASTKW, q_get_queue_defaults, "get_queue_defaults(qid)\n\nReturn the queue's default values, set when it was created."),
        ("is_full", FASTKW, q_is_full, "is_full(qid)\n\nReturn true if the queue has a maxsize and has reached it."),
        ("get_count", FASTKW, q_get_count, "get_count(qid)\n\nReturn the number of items in the queue."),
        ("_register_heap_types", FASTKW, q_register_heap_types),
    ];
    let (m, d) = crate::modules::fncommon::new_module("_interpqueues", methods, Some("This module provides primitive operations to manage Python interpreters.\nThe 'interpreters' module provides a more convenient interface."))?;
    crate::types::dict::set_item_str(&d, "QueueError", crate::types::typeobject::type_obj(QUEUE_ERROR.load(Acquire)))?;
    crate::types::dict::set_item_str(&d, "QueueNotFoundError", crate::types::typeobject::type_obj(QUEUE_NOT_FOUND.load(Acquire)))?;
    Ok(m)
}
