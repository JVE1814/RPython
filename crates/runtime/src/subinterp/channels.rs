//! `_interpchannels`: process-wide channels between interpreters.
//!
//! A channel is a FIFO with a send end and a receive end.  Each interpreter
//! that uses an end becomes associated with it; an interpreter can release its
//! association, and once every association of a used channel has been
//! released, the channel is closed.  `close()` closes a channel for everybody
//! (a channel whose send end is closed while items remain stays readable until
//! it is empty).  `send()` can wait until its item has been received.

use super::xidata::{self, Mode, Xi};
use super::InterpPtr;
use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::{bool_, dict, int, list, none, str as pystr, tuple};
use crate::{mdefs, raise};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicPtr, Ordering::*};
use std::sync::{Arc, Condvar, Mutex};

const UNBOUND_REMOVE: i32 = 1;
const UNBOUND_REPLACE: i32 = 3;

/// What happened to a sent item (for a sender waiting on it).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fate {
    Pending,
    Received,
    Dropped,
}

struct Waiter {
    fate: Mutex<Fate>,
    cv: Condvar,
}

impl Waiter {
    fn settle(&self, f: Fate) {
        let mut g = self.fate.lock().unwrap();
        if *g == Fate::Pending {
            *g = f;
        }
        self.cv.notify_all();
    }
}

struct Item {
    data: Option<Xi>,
    interp: i64,
    unboundop: i32,
    unbound: bool,
    waiter: Option<Arc<Waiter>>,
}

impl Drop for Item {
    fn drop(&mut self) {
        if let Some(w) = &self.waiter {
            w.settle(Fate::Dropped);
        }
    }
}

/// An interpreter's association with one end.
struct Assoc {
    interp: i64,
    open: bool,
}

struct Channel {
    items: VecDeque<Item>,
    send: Vec<Assoc>,
    recv: Vec<Assoc>,
    /// false once every association was released
    open: bool,
    /// the send end was closed while items remained
    closing: bool,
}

impl Channel {
    fn ends_open(&self) -> bool {
        if self.send.is_empty() && self.recv.is_empty() {
            return true; // never used
        }
        self.send.iter().any(|a| a.open) || self.recv.iter().any(|a| a.open)
    }
    fn associate(&mut self, send: bool, interp: i64) -> bool {
        let ends = if send { &mut self.send } else { &mut self.recv };
        match ends.iter().find(|a| a.interp == interp) {
            Some(a) => a.open,
            None => {
                ends.push(Assoc { interp, open: true });
                true
            }
        }
    }
    fn release(&mut self, send: bool, interp: i64) {
        let ends = if send { &mut self.send } else { &mut self.recv };
        match ends.iter_mut().find(|a| a.interp == interp) {
            Some(a) => a.open = false,
            None => ends.push(Assoc { interp, open: false }),
        }
    }
}

struct Ref {
    id: i64,
    /// live ChannelID objects (in any interpreter); the channel goes away with
    /// the last one
    objcount: i64,
    /// None once closed with close()
    chan: Option<Channel>,
    unboundop: i32,
    fallback: i32,
}

struct Registry {
    refs: Vec<Ref>,
    next_id: i64,
}

static REG: Mutex<Registry> = Mutex::new(Registry { refs: Vec::new(), next_id: 1 });

fn reg() -> std::sync::MutexGuard<'static, Registry> {
    REG.lock().unwrap_or_else(|e| e.into_inner())
}

// --------------------------------------------------------------------- errors

static ERRORS: [AtomicPtr<PyTypeObject>; 5] = [const { AtomicPtr::new(std::ptr::null_mut()) }; 5];
const E_BASE: usize = 0;
const E_NOT_FOUND: usize = 1;
const E_CLOSED: usize = 2;
const E_EMPTY: usize = 3;
const E_NOT_EMPTY: usize = 4;

fn ensure_types() {
    if !ERRORS[E_NOT_EMPTY].load(Acquire).is_null() {
        return;
    }
    static LOCK: Mutex<()> = Mutex::new(());
    let _g = LOCK.lock().unwrap();
    if !ERRORS[E_NOT_EMPTY].load(Acquire).is_null() {
        return;
    }
    super::with_interp(super::main(), || {
        let saved = take_exc();
        let mk = |name: &str, base: *mut PyTypeObject| -> *mut PyTypeObject {
            let t = crate::modules::devtools::new_exception("_interpchannels", name, base).expect("channel error type");
            t.into_raw() as *mut PyTypeObject
        };
        let base = mk("ChannelError", crate::types::exceptions::RuntimeError());
        let nf = mk("ChannelNotFoundError", base);
        let cl = mk("ChannelClosedError", base);
        let em = mk("ChannelEmptyError", base);
        let ne = mk("ChannelNotEmptyError", base);
        clear_exc();
        for (i, t) in [base, nf, cl, em, ne].into_iter().enumerate() {
            ERRORS[i].store(t, Release);
        }
        if let Some(e) = saved {
            set_exc(e);
        }
    });
}

fn chan_err(kind: usize, msg: String) -> PyErr {
    ensure_types();
    new_err(ERRORS[kind].load(Acquire), &msg)
}
fn not_found(id: i64) -> PyErr {
    chan_err(E_NOT_FOUND, format!("channel {} not found", id))
}
fn closed(id: i64) -> PyErr {
    chan_err(E_CLOSED, format!("channel {} is closed", id))
}
fn empty(id: i64) -> PyErr {
    chan_err(E_EMPTY, format!("channel {} is empty", id))
}

/// Run `f` on an open channel (errors: not found, closed).
fn with_open<R>(id: i64, f: impl FnOnce(&mut Channel) -> PyResult<R>) -> PyResult<R> {
    let mut g = reg();
    let Some(r) = g.refs.iter_mut().find(|r| r.id == id) else {
        drop(g);
        return Err(not_found(id));
    };
    match r.chan.as_mut() {
        Some(c) if c.open => f(c),
        _ => {
            drop(g);
            Err(closed(id))
        }
    }
}

// --------------------------------------------------------------------- ChannelID

/// ChannelID is a small Python class (one per interpreter, like any heap type).
const CHANNEL_ID_SRC: &str = r#"
class ChannelID:
    """A channel ID (an int) and the end of the channel it refers to."""

    __slots__ = ('_cid', '_end', '_counted')
    __module__ = '_interpchannels'

    def __new__(cls, *args, **kwargs):
        raise TypeError('cannot create _interpchannels.ChannelID instances directly')

    @classmethod
    def _make(cls, cid, end='both'):
        self = object.__new__(cls)
        object.__setattr__(self, '_cid', cid)
        object.__setattr__(self, '_end', end)
        # a live ChannelID keeps its channel alive
        object.__setattr__(self, '_counted', _cid_acquire(cid))
        return self

    def __setattr__(self, name, value):
        raise AttributeError(f'readonly attribute {name!r}')

    # (tail3) everything __del__ needs is bound at definition time: an
    # interpreter being torn down may already have cleared this module's
    # globals (and its __builtins__) when the last ChannelID goes away.
    def __del__(self, _getattr=getattr, _setattr=object.__setattr__,
                _release=_cid_release):
        if _getattr(self, '_counted', False):
            _setattr(self, '_counted', False)
            _release(self._cid)

    def __reduce__(self):
        return (_channel_id, (self._cid,), None)

    def __int__(self):
        return self._cid

    def __index__(self):
        return self._cid

    def __hash__(self):
        return hash(self._cid)

    def __eq__(self, other):
        if isinstance(other, ChannelID):
            return self._cid == other._cid
        if isinstance(other, (int, float)) and not isinstance(other, bool):
            return self._cid == other
        return NotImplemented

    def __repr__(self):
        if self._end == 'send':
            return f'ChannelID({self._cid}, send=True)'
        if self._end == 'recv':
            return f'ChannelID({self._cid}, recv=True)'
        return f'ChannelID({self._cid})'

    def __str__(self):
        return str(self._cid)

    @property
    def end(self):
        """'send', 'recv', or 'both'"""
        return self._end

    @property
    def send(self):
        """the 'send' end of the channel"""
        return ChannelID._make(self._cid, 'send')

    @property
    def recv(self):
        """the 'recv' end of the channel"""
        return ChannelID._make(self._cid, 'recv')
"#;

static CHANNEL_ID_TYPE: InterpPtr = InterpPtr::new();
/// The (SendChannel, RecvChannel) classes registered in this interpreter.
static END_TYPES: InterpPtr = InterpPtr::new();

fn channel_id_type() -> PyResult {
    let p = CHANNEL_ID_TYPE.load(Acquire);
    if p.is_null() {
        crate::import::import_absolute("_interpchannels")?;
        let p = CHANNEL_ID_TYPE.load(Acquire);
        if p.is_null() {
            return Err(system_error("_interpchannels.ChannelID is not available"));
        }
        return Ok(unsafe { PyRef::from_borrowed(p) });
    }
    Ok(unsafe { PyRef::from_borrowed(p) })
}

const END_NAMES: [&str; 3] = ["both", "send", "recv"];

fn new_channel_id(id: i64, end: i32) -> PyResult {
    let t = channel_id_type()?;
    let mk = ab::getattr_str(&t, "_make")?;
    ab::call(&mk, &[&int::new_int(id), &pystr::new_str(END_NAMES[end.clamp(0, 2) as usize])])
}

/// (id, end) of a ChannelID of the current interpreter.
pub fn channel_id_of(o: &PyObject) -> Option<(i64, i32)> {
    let p = CHANNEL_ID_TYPE.load(Acquire);
    if p.is_null() || !o.type_ptr().eq(&(p as *mut PyTypeObject)) {
        return None;
    }
    let get = || -> PyResult<(i64, i32)> {
        let id = int::as_i64(&*ab::getattr_str(o, "_cid")?).unwrap_or(0);
        let end = pystr::to_string_lossy(&*ab::getattr_str(o, "_end")?);
        Ok((id, END_NAMES.iter().position(|e| *e == end).unwrap_or(0) as i32))
    };
    match get() {
        Ok(v) => Some(v),
        Err(_) => {
            clear_exc();
            None
        }
    }
}

pub fn channel_id_object(id: i64, end: i32) -> PyResult {
    new_channel_id(id, end)
}

/// (id, is_send) of an instance of a registered end type.
pub fn end_id_of(o: &PyObject) -> PyResult<Option<(i64, bool)>> {
    let p = END_TYPES.load(Acquire);
    if p.is_null() {
        return Ok(None);
    }
    let types = unsafe { &*p };
    let (st, rt) = (tuple::tuple_get(types, 0), tuple::tuple_get(types, 1));
    let send = if o.type_ptr() == st.as_ptr() as *mut PyTypeObject {
        true
    } else if o.type_ptr() == rt.as_ptr() as *mut PyTypeObject {
        false
    } else {
        return Ok(None);
    };
    let cid = ab::getattr_str(o, "_id")?;
    match channel_id_of(&cid) {
        Some((id, _)) => Ok(Some((id, send))),
        None => raise!(TypeError, "'_id' attribute must be an _interpchannels.ChannelID object, not {}", crate::types::typeobject::tname(&cid)),
    }
}

pub fn end_object(id: i64, send: bool) -> PyResult {
    let mut p = END_TYPES.load(Acquire);
    if p.is_null() {
        crate::import::import_absolute("test.support.channels")?;
        p = END_TYPES.load(Acquire);
        if p.is_null() {
            return Err(system_error("channel end types not registered"));
        }
    }
    let types = unsafe { PyRef::from_borrowed(p) };
    let t = tuple::tuple_get(&types, if send { 0 } else { 1 });
    let cid = new_channel_id(id, if send { 1 } else { 2 })?;
    ab::call(t, &[&cid])
}

// --------------------------------------------------------------------- arguments

fn id_arg(o: &PyObject) -> PyResult<i64> {
    if let Some((id, _)) = channel_id_of(o) {
        return Ok(id);
    }
    if pystr::is_str_exact(o) {
        raise!(TypeError, "channel ID must be an int, got {}", crate::types::typeobject::tname(o));
    }
    let i = match ab::index(o) {
        Ok(i) => i,
        Err(_) => {
            clear_exc();
            raise!(TypeError, "channel ID must be an int, got {}", crate::types::typeobject::tname(o));
        }
    };
    if int::sign(&i) < 0 {
        raise!(ValueError, "channel ID must be a non-negative int, got {}", ab::repr_string(&i)?);
    }
    match int::as_i64(&i) {
        Some(v) => Ok(v),
        None => raise!(OverflowError, "channel ID too large"),
    }
}

fn opt_bool(o: Option<&PyObject>) -> PyResult<Option<bool>> {
    match o {
        Some(v) if !v.is(none::none()) => Ok(Some(ab::is_true(v)?)),
        _ => Ok(None),
    }
}

fn int_opt(o: Option<&PyObject>, default: i64) -> PyResult<i64> {
    match o {
        Some(v) if !v.is(none::none()) => Ok(int::as_i64(&*ab::index(v)?).unwrap_or(default)),
        _ => Ok(default),
    }
}

fn cur_interp() -> i64 {
    super::current().id
}

// --------------------------------------------------------------------- operations

fn c_create(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("create", &["unboundop", "fallback"], 0, 0);
    let [op, fb] = args::parse::<2>(&P, args, kw)?;
    let mut op = int_opt(op, -1)? as i32;
    if op == -1 {
        op = UNBOUND_REPLACE;
    }
    if !(1..=3).contains(&op) {
        raise!(ValueError, "unsupported unboundop {}", op);
    }
    let fallback = int_opt(fb, -1)? as i32;
    let id = {
        let mut g = reg();
        let id = g.next_id;
        g.next_id += 1;
        g.refs.push(Ref { id, objcount: 0, chan: Some(Channel { items: VecDeque::new(), send: Vec::new(), recv: Vec::new(), open: true, closing: false }), unboundop: op, fallback });
        id
    };
    new_channel_id(id, 0)
}

fn c_destroy(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("destroy", &["cid"], 0, 1);
    let [cid] = args::parse::<1>(&P, args, kw)?;
    let id = id_arg(cid.unwrap())?;
    let removed = {
        let mut g = reg();
        g.refs.iter().position(|r| r.id == id).map(|i| g.refs.remove(i))
    };
    match removed {
        Some(r) => {
            drop(r);
            Ok(none::none_ref())
        }
        None => Err(not_found(id)),
    }
}

fn c_list_all(_m: &PyObject) -> PyResult {
    let v: Vec<(i64, i32, i32)> = reg().refs.iter().filter(|r| r.chan.is_some()).map(|r| (r.id, r.unboundop, r.fallback)).collect();
    let mut out = Vec::new();
    for (id, op, fb) in v {
        out.push(tuple::new_tuple(vec![new_channel_id(id, 0)?, int::new_int(op as i64), int::new_int(fb as i64)]));
    }
    Ok(list::new_list(out))
}

fn c_list_interpreters(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let mut p = Params::new("list_interpreters", &["cid", "send"], 0, 2);
    p.kwonly = 1;
    p.kwonly_required = &["send"];
    let [cid, send] = args::parse::<2>(&p, args, kw)?;
    let Some(send) = send else { raise!(TypeError, "list_interpreters() missing required keyword-only argument: 'send'") };
    let send = ab::is_true(send)?;
    let id = id_arg(cid.unwrap())?;
    let ids = with_open(id, |c| {
        if send && c.closing {
            return Err(closed(id));
        }
        let ends = if send { &c.send } else { &c.recv };
        Ok(ends.iter().filter(|a| a.open).map(|a| a.interp).collect::<Vec<i64>>())
    })?;
    Ok(list::new_list(ids.into_iter().map(int::new_int).collect()))
}

/// Queue data; returns the waiter when the sender wants to wait.
fn push(id: i64, data: Xi, unboundop: i32, wait: bool) -> PyResult<Option<Arc<Waiter>>> {
    let interp = cur_interp();
    let mut g = reg();
    let Some(r) = g.refs.iter_mut().find(|r| r.id == id) else {
        drop(g);
        return Err(not_found(id));
    };
    let default_op = r.unboundop;
    let Some(c) = r.chan.as_mut().filter(|c| c.open && !c.closing) else {
        drop(g);
        return Err(closed(id));
    };
    if !c.associate(true, interp) {
        drop(g);
        return Err(closed(id));
    }
    let waiter = if wait { Some(Arc::new(Waiter { fate: Mutex::new(Fate::Pending), cv: Condvar::new() })) } else { None };
    let op = if unboundop == -1 { default_op } else { unboundop };
    c.items.push_back(Item { data: Some(data), interp, unboundop: op, unbound: false, waiter: waiter.clone() });
    Ok(waiter)
}

fn send_common(fname: &'static str, args: &[&PyObject], kw: Option<&PyObject>, buffer: bool) -> PyResult {
    let p = Params::new(fname, &["cid", "obj", "unboundop", "blocking", "timeout"], 0, 2).kwonly(2);
    let [cid, obj, op, blocking, timeout] = args::parse::<5>(&p, args, kw)?;
    let id = id_arg(cid.unwrap())?;
    let op = int_opt(op, -1)? as i32;
    let blocking = match blocking {
        Some(b) => ab::is_true(b)?,
        None => true,
    };
    let timeout = match timeout.filter(|t| !t.is(none::none())) {
        Some(t) => {
            if !blocking {
                raise!(ValueError, "timeout not supported if blocking=False");
            }
            let v = ab::float_value(t)?;
            if v < 0.0 {
                raise!(ValueError, "timeout value must be non-negative");
            }
            Some(std::time::Duration::from_secs_f64(v))
        }
        None => None,
    };
    let obj = obj.unwrap();
    // fail early on a missing or closed channel
    with_open(id, |_| Ok(()))?;
    let data = if buffer {
        let mv = crate::types::typeobject::type_obj(crate::types::memoryview::memoryview_type());
        let view = ab::call(mv, &[obj])?;
        Xi::Buffer(view)
    } else {
        xidata::get(obj, Mode::Default)?
    };
    let Some(w) = push(id, data, op, blocking)? else { return Ok(none::none_ref()) };
    // wait until the item is received (or dropped, or the timeout passes)
    let deadline = timeout.map(|t| std::time::Instant::now() + t);
    let fate = crate::threads::blocking(|| {
        let mut g = w.fate.lock().unwrap();
        loop {
            if *g != Fate::Pending {
                return *g;
            }
            match deadline {
                Some(d) => {
                    let now = std::time::Instant::now();
                    if now >= d {
                        return Fate::Pending;
                    }
                    g = w.cv.wait_timeout(g, d - now).unwrap().0;
                }
                None => g = w.cv.wait(g).unwrap(),
            }
        }
    });
    match fate {
        Fate::Received => Ok(none::none_ref()),
        Fate::Dropped => Err(closed(id)),
        Fate::Pending => {
            // timed out: take the item back
            let removed = {
                let mut g = reg();
                g.refs.iter_mut().find(|r| r.id == id).and_then(|r| r.chan.as_mut()).and_then(|c| {
                    c.items.iter().position(|it| it.waiter.as_ref().is_some_and(|x| Arc::ptr_eq(x, &w))).map(|i| c.items.remove(i))
                })
            };
            drop(removed);
            if *w.fate.lock().unwrap() == Fate::Received {
                return Ok(none::none_ref());
            }
            raise!(TimeoutError, "timed out");
        }
    }
}

fn c_send(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    send_common("send", args, kw, false)
}
fn c_send_buffer(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    send_common("send_buffer", args, kw, true)
}

fn c_recv(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("recv", &["cid", "default"], 0, 1);
    let [cid, default] = args::parse::<2>(&P, args, kw)?;
    let id = id_arg(cid.unwrap())?;
    let interp = cur_interp();
    let item = {
        let mut g = reg();
        let Some(r) = g.refs.iter_mut().find(|r| r.id == id) else {
            drop(g);
            return Err(not_found(id));
        };
        let Some(c) = r.chan.as_mut().filter(|c| c.open) else {
            drop(g);
            return Err(closed(id));
        };
        if !c.associate(false, interp) {
            drop(g);
            return Err(closed(id));
        }
        let it = c.items.pop_front();
        if it.is_none() && c.closing {
            // the send end was closed and everything has been received
            r.chan = None;
            drop(g);
            return Err(closed(id));
        }
        it
    };
    let Some(mut item) = item else {
        return match default {
            Some(d) => Ok(tuple::new_tuple(vec![d.to_ref(), none::none_ref()])),
            None => Err(empty(id)),
        };
    };
    if let Some(w) = item.waiter.take() {
        w.settle(Fate::Received);
    }
    if item.unbound {
        return Ok(tuple::new_tuple(vec![none::none_ref(), int::new_int(item.unboundop as i64)]));
    }
    let o = item.data.as_ref().unwrap().to_object()?;
    Ok(tuple::new_tuple(vec![o, none::none_ref()]))
}

fn c_close(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("close", &["cid", "send", "recv", "force"], 0, 1).kwonly(3);
    let [cid, send, recv, force] = args::parse::<4>(&P, args, kw)?;
    let id = id_arg(cid.unwrap())?;
    let send = opt_bool(send)?.unwrap_or(false);
    let recv = opt_bool(recv)?.unwrap_or(false);
    let force = opt_bool(force)?.unwrap_or(false);
    let dropped = {
        let mut g = reg();
        let Some(r) = g.refs.iter_mut().find(|r| r.id == id) else {
            drop(g);
            return Err(not_found(id));
        };
        let Some(c) = r.chan.as_mut().filter(|c| c.open && !c.closing) else {
            drop(g);
            return Err(closed(id));
        };
        if !c.items.is_empty() && !force {
            if send && !recv {
                // closed for sending; readable until empty
                c.closing = true;
                return Ok(none::none_ref());
            }
            drop(g);
            return Err(chan_err(E_NOT_EMPTY, format!("channel {} may not be closed if not empty (try force=True)", id)));
        }
        r.chan.take()
    };
    drop(dropped);
    Ok(none::none_ref())
}

fn c_release(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("release", &["cid", "send", "recv", "force"], 0, 1).kwonly(3);
    let [cid, send, recv, _force] = args::parse::<4>(&P, args, kw)?;
    let id = id_arg(cid.unwrap())?;
    let mut send = opt_bool(send)?.unwrap_or(false);
    let mut recv = opt_bool(recv)?.unwrap_or(false);
    if !send && !recv {
        send = true;
        recv = true;
    }
    let interp = cur_interp();
    let dropped = {
        let mut g = reg();
        let Some(r) = g.refs.iter_mut().find(|r| r.id == id) else {
            drop(g);
            return Err(not_found(id));
        };
        let Some(c) = r.chan.as_mut().filter(|c| c.open) else {
            drop(g);
            return Err(closed(id));
        };
        if send {
            c.release(true, interp);
        }
        if recv {
            c.release(false, interp);
        }
        c.open = c.ends_open();
        if !c.open { std::mem::take(&mut c.items) } else { VecDeque::new() }
    };
    drop(dropped);
    Ok(none::none_ref())
}

fn c_get_count(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("get_count", &["cid"], 0, 1);
    let [cid] = args::parse::<1>(&P, args, kw)?;
    let id = id_arg(cid.unwrap())?;
    let n = {
        let g = reg();
        match g.refs.iter().find(|r| r.id == id) {
            Some(r) => r.chan.as_ref().map_or(0, |c| c.items.len()),
            None => {
                drop(g);
                return Err(not_found(id));
            }
        }
    };
    Ok(int::new_int(n as i64))
}

fn c_get_info(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("get_info", &["cid"], 0, 1);
    let [cid] = args::parse::<1>(&P, args, kw)?;
    let id = id_arg(cid.unwrap())?;
    let interp = cur_interp();
    let (closed_, closing, count, nsend, nrecv, send_assoc, recv_assoc) = {
        let g = reg();
        let Some(r) = g.refs.iter().find(|r| r.id == id) else {
            drop(g);
            return Err(not_found(id));
        };
        match &r.chan {
            None => (true, false, 0, 0, 0, 0i64, 0i64),
            Some(c) => {
                let mine = |ends: &Vec<Assoc>| ends.iter().find(|a| a.interp == interp).map_or(0, |a| if a.open { 1 } else { -1 });
                (!c.open, c.closing, c.items.len(), c.send.iter().filter(|a| a.open).count(), c.recv.iter().filter(|a| a.open).count(), mine(&c.send), mine(&c.recv))
            }
        }
    };
    let b = bool_::new_bool;
    xidata::namespace(&[
        ("open", b(!closed_)),
        ("closing", b(closing)),
        ("closed", b(closed_)),
        ("count", int::new_int(count as i64)),
        ("num_interp_send", int::new_int(nsend as i64)),
        ("num_interp_recv", int::new_int(nrecv as i64)),
        ("send_associated", b(send_assoc != 0)),
        ("send_released", b(send_assoc < 0)),
        ("recv_associated", b(recv_assoc != 0)),
        ("recv_released", b(recv_assoc < 0)),
    ])
}

fn c_get_defaults(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("get_channel_defaults", &["cid"], 0, 1);
    let [cid] = args::parse::<1>(&P, args, kw)?;
    let id = id_arg(cid.unwrap())?;
    let (op, fb) = {
        let g = reg();
        match g.refs.iter().find(|r| r.id == id) {
            Some(r) => (r.unboundop, r.fallback),
            None => {
                drop(g);
                return Err(not_found(id));
            }
        }
    };
    Ok(tuple::new_tuple(vec![int::new_int(op as i64), int::new_int(fb as i64)]))
}

fn c_channel_id(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("_channel_id", &["id", "send", "recv", "force", "_resolve"], 0, 1).kwonly(4);
    let [cid, send, recv, force, _resolve] = args::parse::<5>(&P, args, kw)?;
    let id = id_arg(cid.unwrap())?;
    let send = opt_bool(send)?;
    let recv = opt_bool(recv)?;
    let end = match (send, recv) {
        (Some(false), Some(false)) => raise!(ValueError, "'send' and 'recv' cannot both be False"),
        (Some(true), Some(true)) | (None, None) => 0,
        (Some(true), _) | (_, Some(false)) => 1,
        (_, Some(true)) | (Some(false), _) => 2,
    };
    if !opt_bool(force)?.unwrap_or(false) && !reg().refs.iter().any(|r| r.id == id) {
        return Err(not_found(id));
    }
    new_channel_id(id, end)
}

/// A ChannelID object was created: count it if its channel exists.
fn c_cid_acquire(_m: &PyObject, o: &PyObject) -> PyResult {
    let id = int::as_i64(&*ab::index(o)?).unwrap_or(-1);
    let mut g = reg();
    let found = match g.refs.iter_mut().find(|r| r.id == id) {
        Some(r) => {
            r.objcount += 1;
            true
        }
        None => false,
    };
    Ok(bool_::new_bool(found))
}

/// A counted ChannelID object went away.
fn c_cid_release(_m: &PyObject, o: &PyObject) -> PyResult {
    let id = int::as_i64(&*ab::index(o)?).unwrap_or(-1);
    let removed = {
        let mut g = reg();
        match g.refs.iter().position(|r| r.id == id) {
            Some(i) => {
                g.refs[i].objcount -= 1;
                if g.refs[i].objcount <= 0 { Some(g.refs.remove(i)) } else { None }
            }
            None => None,
        }
    };
    drop(removed);
    Ok(none::none_ref())
}

fn c_register_end_types(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("_register_end_types", &["send", "recv"], 0, 2);
    let [s, r] = args::parse::<2>(&P, args, kw)?;
    for t in [s, r] {
        if !crate::types::typeobject::is_type(t.unwrap()) {
            raise!(TypeError, "expected a type, got {}", crate::types::typeobject::tname(t.unwrap()));
        }
    }
    let t = tuple::new_tuple(vec![s.unwrap().to_ref(), r.unwrap().to_ref()]);
    let old = END_TYPES.swap(t.into_raw(), AcqRel);
    unsafe { xdecref(old) };
    Ok(none::none_ref())
}

/// An interpreter is gone: its associations close, its items become unbound
/// (or are removed), and channels left without open associations close.
pub fn interp_destroyed(interp: i64) {
    let mut dropped: Vec<Item> = Vec::new();
    {
        let mut g = reg();
        for r in g.refs.iter_mut() {
            let Some(c) = r.chan.as_mut() else { continue };
            for a in c.send.iter_mut().chain(c.recv.iter_mut()) {
                if a.interp == interp {
                    a.open = false;
                }
            }
            let mut kept = VecDeque::with_capacity(c.items.len());
            for mut it in c.items.drain(..) {
                if it.interp != interp || it.unbound {
                    kept.push_back(it);
                    continue;
                }
                if it.unboundop == UNBOUND_REMOVE {
                    dropped.push(it);
                    continue;
                }
                it.data = None;
                it.unbound = true;
                kept.push_back(it);
            }
            c.items = kept;
            if c.open && !c.ends_open() {
                c.open = false;
            }
        }
    }
    drop(dropped);
}

pub fn init_module() -> PyResult {
    ensure_types();
    let methods = mdefs![
        ("create", FASTKW, c_create, "channel_create(unboundop, fallback) -> cid\n\nCreate a new cross-interpreter channel and return a unique generated ID."),
        ("destroy", FASTKW, c_destroy, "channel_destroy(cid)\n\nClose and finalize the channel.  Afterward attempts to use the channel\nwill behave as though it never existed."),
        ("list_all", NOARGS, c_list_all, "channel_list_all() -> [(cid, unboundop, fallback)]\n\nReturn the list of all IDs for active channels."),
        ("list_interpreters", FASTKW, c_list_interpreters, "channel_list_interpreters(cid, *, send) -> [id]\n\nReturn the list of all interpreter IDs associated with an end of the channel."),
        ("send", FASTKW, c_send, "channel_send(cid, obj, *, blocking=True, timeout=None)\n\nAdd the object's data to the channel's queue."),
        ("send_buffer", FASTKW, c_send_buffer, "channel_send_buffer(cid, obj, *, blocking=True, timeout=None)\n\nAdd the object's buffer to the channel's queue."),
        ("recv", FASTKW, c_recv, "channel_recv(cid, [default]) -> (obj, unboundop)\n\nReturn a new object from the data at the front of the channel's queue."),
        ("close", FASTKW, c_close, "channel_close(cid, *, send=None, recv=None, force=False)\n\nClose the channel for all interpreters."),
        ("release", FASTKW, c_release, "channel_release(cid, *, send=None, recv=None, force=True)\n\nClose the channel for the current interpreter."),
        ("get_count", FASTKW, c_get_count, "get_count(cid)\n\nReturn the number of items in the channel."),
        ("get_info", FASTKW, c_get_info, "get_info(cid)\n\nReturn details about the channel."),
        ("get_channel_defaults", FASTKW, c_get_defaults, "get_channel_defaults(cid)\n\nReturn the channel's default values, set when it was created."),
        ("get_queue_defaults", FASTKW, c_get_defaults),
        ("_channel_id", FASTKW, c_channel_id),
        ("_register_end_types", FASTKW, c_register_end_types),
        ("_cid_acquire", O, c_cid_acquire),
        ("_cid_release", O, c_cid_release),
    ];
    let (m, d) = crate::modules::fncommon::new_module("_interpchannels", methods, Some("This module provides primitive operations to manage Python interpreters.\nThe 'interpreters' module provides a more convenient interface."))?;
    for (i, n) in ["ChannelError", "ChannelNotFoundError", "ChannelClosedError", "ChannelEmptyError", "ChannelNotEmptyError"].iter().enumerate() {
        dict::set_item_str(&d, n, crate::types::typeobject::type_obj(ERRORS[i].load(Acquire)))?;
    }
    dict::set_item_str(&d, "__builtins__", &crate::builtins::builtins_module())?;
    let code = crate::compile::compile_str(CHANNEL_ID_SRC, "<_interpchannels>", crate::compile::Mode::Exec, 0, -1)?;
    crate::interp::eval_code(&code.code, &d, Some(&d))?;
    let t = dict::get_item_rs(&d, "ChannelID").unwrap();
    let old = CHANNEL_ID_TYPE.swap(t.into_raw(), AcqRel);
    unsafe { xdecref(old) };
    Ok(m)
}
