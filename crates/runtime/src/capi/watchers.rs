//! Watchers (cpython/dictobject.h, object.h, funcobject.h, code.h, context.h): C
//! callbacks notified when watched dicts or types change, and when functions, code
//! objects or context switches happen.
//!
//! Each kind has 8 callback slots. The runtime's hot paths only test one relaxed
//! counter (`ACTIVE_*`, the number of watched dicts / registered callbacks) before
//! doing any work, so watchers cost nothing while unused. A callback that fails is
//! reported as unraisable; the operation that triggered it is not affected.

use super::*;
use crate::types::{dict as tdict, str as pystr};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::*};

const MAX: usize = 8;

struct Registry {
    slots: Mutex<[usize; MAX]>,
    /// registered callbacks (dicts/types: watched objects, see below)
    active: AtomicUsize,
    what: &'static str,
    /// first ID handed to users (lower IDs are reserved for the runtime)
    first: usize,
}

impl Registry {
    const fn new(what: &'static str, first: usize) -> Registry {
        Registry { slots: Mutex::new([0; MAX]), active: AtomicUsize::new(0), what, first }
    }
    fn add(&self, cb: *mut c_void) -> c_int {
        let mut g = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        for i in self.first..MAX {
            if g[i] == 0 {
                g[i] = cb as usize;
                self.active.fetch_add(1, SeqCst);
                return i as c_int;
            }
        }
        let _ = crate::err!(RuntimeError, "no more {} watcher IDs available", self.what);
        -1
    }
    /// Validate `id`: Err after setting ValueError. `lower`: the lower-case wording.
    fn check(&self, id: c_int, lower: bool) -> Result<(), ()> {
        let g = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let (inv, no) = if lower { ("invalid", "no") } else { ("Invalid", "No") };
        if !(0..MAX as c_int).contains(&id) {
            let _ = value_error(&format!("{} {} watcher ID {}", inv, self.what, id));
            return Err(());
        }
        if g[id as usize] == 0 {
            let _ = value_error(&format!("{} {} watcher set for ID {}", no, self.what, id));
            return Err(());
        }
        Ok(())
    }
    fn clear(&self, id: c_int, lower: bool) -> c_int {
        if self.check(id, lower).is_err() {
            return -1;
        }
        let mut g = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if g[id as usize] != 0 {
            g[id as usize] = 0;
            self.active.fetch_sub(1, SeqCst);
        }
        0
    }
    fn callbacks(&self, mask: u8) -> Vec<(usize, usize)> {
        let g = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        (0..MAX).filter(|&i| mask & (1 << i) != 0 && g[i] != 0).map(|i| (i, g[i])).collect()
    }
}

static TYPES: Registry = Registry::new("type", 1);
static FUNCS: Registry = Registry::new("func", 0);
static CODES: Registry = Registry::new("code", 0);
static CONTEXTS: Registry = Registry::new("context", 0);

/// Run callbacks with the pending exception (if any) put aside; a failing callback is
/// reported with `msg()` as unraisable.
fn dispatch(cbs: Vec<(usize, usize)>, call: &mut dyn FnMut(usize) -> c_int, msg: &dyn Fn(usize) -> String) {
    if cbs.is_empty() {
        return;
    }
    let saved = take_exc();
    for (id, cb) in cbs {
        if call(cb) < 0 {
            crate::modules::sys_full::write_unraisable(Some(&msg(id)), None);
        }
        clear_exc();
    }
    if let Some(e) = saved {
        set_exc(e);
    }
}

fn repr_or(o: &PyObject, fallback: impl Fn() -> String) -> String {
    let saved = take_exc();
    let r = crate::abstract_::repr_string(o).unwrap_or_else(|_| {
        clear_exc();
        fallback()
    });
    if let Some(e) = saved {
        set_exc(e);
    }
    r
}

// ---------------------------------------------------------------- dicts
// (dict watchers: capi/dict.rs, wheels2)

// ---------------------------------------------------------------- types

#[inline(always)]
pub fn type_watched(tp: *mut PyTypeObject) -> bool {
    unsafe { (*tp).tp_watched != 0 }
}

thread_local! {
    /// Watched types modified while a runtime lock may be held (a type dict is written
    /// under its lock): reported by `flush_type_events`.
    static PENDING_TYPES: std::cell::RefCell<Vec<PyRef>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A watched type was modified: the watchers run at the next `flush_type_events`.
#[cold]
pub fn type_event(tp: *mut PyTypeObject) {
    let t = unsafe { PyRef::from_borrowed(tp as *mut PyObject) };
    PENDING_TYPES.with(|p| p.borrow_mut().push(t));
}

/// Report queued type modifications (call where no runtime lock is held).
pub fn flush_type_events() {
    loop {
        let next = PENDING_TYPES.with(|p| {
            let mut v = p.borrow_mut();
            if v.is_empty() { None } else { Some(v.remove(0)) }
        });
        match next {
            Some(t) => type_event_now(t.as_ptr() as *mut PyTypeObject),
            None => break,
        }
    }
}

/// Run the watchers of `tp` now (a modification, or the type being deallocated).
#[cold]
pub fn type_event_now(tp: *mut PyTypeObject) {
    type_event_impl(tp, false)
}

fn type_event_impl(tp: *mut PyTypeObject, dying: bool) {
    let bits = unsafe { (*tp).tp_watched } & current_type_bits(tp);
    let cbs = TYPES.callbacks(bits);
    dispatch(
        cbs,
        &mut |cb| unsafe {
            let f: unsafe extern "C" fn(*mut PyTypeObject) -> c_int = std::mem::transmute(cb);
            f(tp)
        },
        &|id| {
            // a dying type cannot be passed to repr() (it has no references left)
            let named = || format!("<class '{}'>", unsafe { std::ffi::CStr::from_ptr((*tp).tp_name) }.to_string_lossy());
            let r = if dying { named() } else { repr_or(unsafe { &*(tp as *mut PyObject) }, named) };
            format!("Exception ignored in type watcher callback #{} for {}", id, r)
        },
    );
}

unsafe fn type_arg(id: c_int, t: *mut PyObject) -> Result<(), ()> {
    unsafe {
        if t.is_null() || !crate::types::typeobject::is_type(&*t) {
            let _ = value_error("Cannot watch non-type");
            return Err(());
        }
        TYPES.check(id, false)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_AddWatcher(cb: *mut c_void) -> c_int {
    TYPES.add(cb)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_ClearWatcher(id: c_int) -> c_int {
    if TYPES.check(id, false).is_ok() {
        // types watched by this ID stop reporting to whoever registers it next
        TYPE_EPOCH.lock().unwrap_or_else(|e| e.into_inner())[id as usize] += 1;
    }
    clear_exc();
    TYPES.clear(id, false)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_Watch(id: c_int, t: *mut PyObject) -> c_int {
    unsafe {
        if type_arg(id, t).is_err() {
            return -1;
        }
        let tp = t as *mut PyTypeObject;
        // the next modification is reported once the type has a version tag
        crate::interp::cache::assign_type_tag(tp);
        (*tp).tp_watched |= 1 << id;
        let ep = TYPE_EPOCH.lock().unwrap_or_else(|e| e.into_inner())[id as usize];
        TYPE_WATCH_EPOCHS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).entry(tp as usize).or_insert([0; MAX])[id as usize] = ep;
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_Unwatch(id: c_int, t: *mut PyObject) -> c_int {
    unsafe {
        if type_arg(id, t).is_err() {
            return -1;
        }
        (*(t as *mut PyTypeObject)).tp_watched &= !(1 << id);
        0
    }
}

/// Per type watcher ID: bumped when the ID's watcher is cleared, so a type watched
/// under an earlier registration of the ID does not report to a later one.
static TYPE_EPOCH: Mutex<[u32; MAX]> = Mutex::new([0; MAX]);
/// type address -> the epoch of each ID when the type started being watched.
static TYPE_WATCH_EPOCHS: Mutex<Option<HashMap<usize, [u32; MAX]>>> = Mutex::new(None);

fn current_type_bits(tp: *mut PyTypeObject) -> u8 {
    let ep = *TYPE_EPOCH.lock().unwrap_or_else(|e| e.into_inner());
    let g = TYPE_WATCH_EPOCHS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(rec) = g.as_ref().and_then(|m| m.get(&(tp as usize))) else { return 0 };
    (0..MAX).filter(|&i| rec[i] == ep[i]).fold(0u8, |b, i| b | (1 << i))
}

/// A watched type is being deallocated: report it, then forget it.
pub fn type_dealloc_event(tp: *mut PyTypeObject) {
    type_event_impl(tp, true);
    let mut g = TYPE_WATCH_EPOCHS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = g.as_mut() {
        m.remove(&(tp as usize));
    }
}

// ---------------------------------------------------------------- functions

pub const FUNC_CREATE: c_int = 0;
pub const FUNC_DESTROY: c_int = 1;
pub const FUNC_MODIFY_CODE: c_int = 2;
pub const FUNC_MODIFY_DEFAULTS: c_int = 3;
pub const FUNC_MODIFY_KWDEFAULTS: c_int = 4;
pub const FUNC_MODIFY_QUALNAME: c_int = 5;
const FUNC_EVENT_NAMES: [&str; 6] = ["CREATE", "DESTROY", "MODIFY_CODE", "MODIFY_DEFAULTS", "MODIFY_KWDEFAULTS", "MODIFY_QUALNAME"];

#[inline(always)]
pub fn funcs_watched() -> bool {
    FUNCS.active.load(Relaxed) != 0
}

#[cold]
pub fn func_event(event: c_int, f: *mut PyObject, new_value: Option<&PyObject>) {
    let cbs = FUNCS.callbacks(0xff);
    let vp = new_value.map_or(std::ptr::null_mut(), |v| v.as_ptr());
    dispatch(
        cbs,
        &mut |cb| unsafe {
            let c: unsafe extern "C" fn(c_int, *mut PyObject, *mut PyObject) -> c_int = std::mem::transmute(cb);
            c(event, f, vp)
        },
        &|_| {
            let r = if event == FUNC_DESTROY {
                format!("function at {:p}", f)
            } else {
                let r = repr_or(unsafe { &*f }, || format!("<function at {:p}>", f));
                r.trim_start_matches('<').trim_end_matches('>').to_string()
            };
            format!("Exception ignored in PyFunction_EVENT_{} watcher callback for {}", FUNC_EVENT_NAMES[event as usize], r)
        },
    );
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFunction_AddWatcher(cb: *mut c_void) -> c_int {
    FUNCS.add(cb)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFunction_ClearWatcher(id: c_int) -> c_int {
    FUNCS.clear(id, true)
}

// ---------------------------------------------------------------- code objects

pub const CODE_CREATE: c_int = 0;
pub const CODE_DESTROY: c_int = 1;

#[inline(always)]
pub fn codes_watched() -> bool {
    CODES.active.load(Relaxed) != 0
}

#[cold]
pub fn code_event(event: c_int, co: *mut PyObject) {
    let cbs = CODES.callbacks(0xff);
    dispatch(
        cbs,
        &mut |cb| unsafe {
            let c: unsafe extern "C" fn(c_int, *mut PyObject) -> c_int = std::mem::transmute(cb);
            c(event, co)
        },
        &|_| {
            let r = if event == CODE_DESTROY { format!("<code object at {:p}>", co) } else { repr_or(unsafe { &*co }, || format!("<code object at {:p}>", co)) };
            format!("Exception ignored in PY_CODE_EVENT_{} watcher callback for {}", if event == CODE_CREATE { "CREATE" } else { "DESTROY" }, r)
        },
    );
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_AddWatcher(cb: *mut c_void) -> c_int {
    CODES.add(cb)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_ClearWatcher(id: c_int) -> c_int {
    CODES.clear(id, false)
}

// ---------------------------------------------------------------- contexts

pub const CONTEXT_SWITCHED: c_int = 1;

#[inline(always)]
pub fn contexts_watched() -> bool {
    CONTEXTS.active.load(Relaxed) != 0
}

/// The current context changed to `ctx` (None: no context).
#[cold]
pub fn context_switched(ctx: &PyObject) {
    let cbs = CONTEXTS.callbacks(0xff);
    let cp = ctx.as_ptr();
    dispatch(
        cbs,
        &mut |cb| unsafe {
            let c: unsafe extern "C" fn(c_int, *mut PyObject) -> c_int = std::mem::transmute(cb);
            c(CONTEXT_SWITCHED, cp)
        },
        &|_| format!("Exception ignored in Py_CONTEXT_SWITCHED watcher callback for {}", repr_or(ctx, || format!("<context at {:p}>", cp))),
    );
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContext_AddWatcher(cb: *mut c_void) -> c_int {
    CONTEXTS.add(cb)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContext_ClearWatcher(id: c_int) -> c_int {
    CONTEXTS.clear(id, false)
}

#[allow(dead_code)]
fn _unused() -> PyRef {
    pystr::new_str("")
}
