//! `_tracemalloc`: trace memory blocks allocated for Python objects (and
//! through the PyMem/PyObject C allocators) together with the Python
//! traceback at allocation time.
//!
//! The allocator calls `track`/`untrack`/`retrack` only while `TRACING` is
//! set, so the cost when disabled is one relaxed load per allocation.  All
//! bookkeeping is plain Rust data behind a mutex (no Python objects), so the
//! hooks never re-enter the object allocator while holding the lock; a
//! per-thread flag additionally ignores allocations made by the hooks
//! themselves.

use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon as fc;
use crate::object::*;
use crate::types::{int, list, none, str as pystr, tuple};
use crate::{mdefs, raise};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering::*};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// True while tracemalloc is tracing (checked by the allocator).
pub static TRACING: AtomicBool = AtomicBool::new(false);

pub const DEFAULT_DOMAIN: u32 = 0;
const MAX_NFRAME: i64 = 65535;
const UNKNOWN_FILENAME: &str = "<unknown>";

#[derive(PartialEq, Eq, Hash)]
struct Traceback {
    /// Most recent frame first.
    frames: Vec<(Arc<str>, i32)>,
    total_nframe: u32,
}

struct Trace {
    size: usize,
    traceback: Arc<Traceback>,
}

struct State {
    max_nframe: usize,
    traces: HashMap<(u32, usize), Trace>,
    tracebacks: HashSet<Arc<Traceback>>,
    filenames: HashSet<Arc<str>>,
    traced: usize,
    peak: usize,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

thread_local! {
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

/// The locked state.  While it is held, this thread's allocations are not
/// traced (they would otherwise try to take the lock again).
struct StateGuard {
    guard: MutexGuard<'static, State>,
    prev_busy: bool,
}

impl std::ops::Deref for StateGuard {
    type Target = State;
    fn deref(&self) -> &State {
        &self.guard
    }
}

impl std::ops::DerefMut for StateGuard {
    fn deref_mut(&mut self) -> &mut State {
        &mut self.guard
    }
}

impl Drop for StateGuard {
    fn drop(&mut self) {
        let prev = self.prev_busy;
        let _ = BUSY.try_with(|b| b.set(prev));
    }
}

fn state() -> StateGuard {
    let prev_busy = BUSY.try_with(|b| b.replace(true)).unwrap_or(true);
    StateGuard { guard: lock_state(), prev_busy }
}

fn lock_state() -> MutexGuard<'static, State> {
    STATE
        .get_or_init(|| {
            Mutex::new(State {
                max_nframe: 1,
                traces: HashMap::new(),
                tracebacks: HashSet::new(),
                filenames: HashSet::new(),
                traced: 0,
                peak: 0,
            })
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Run `f` unless this thread is already inside a tracemalloc hook.
#[inline]
fn guarded(f: impl FnOnce()) {
    let entered = BUSY.try_with(|b| if b.get() { false } else { b.set(true); true }).unwrap_or(false);
    if !entered {
        return;
    }
    f();
    let _ = BUSY.try_with(|b| b.set(false));
}

impl State {
    fn intern_filename(&mut self, name: &str) -> Arc<str> {
        if let Some(a) = self.filenames.get(name) {
            return a.clone();
        }
        let a: Arc<str> = Arc::from(name);
        self.filenames.insert(a.clone());
        a
    }

    fn intern_traceback(&mut self, tb: Traceback) -> Arc<Traceback> {
        if let Some(a) = self.tracebacks.get(&tb) {
            return a.clone();
        }
        let a = Arc::new(tb);
        self.tracebacks.insert(a.clone());
        a
    }

    fn add(&mut self, key: (u32, usize), size: usize, tb: Arc<Traceback>) {
        if let Some(old) = self.traces.insert(key, Trace { size, traceback: tb }) {
            self.traced -= old.size;
        }
        self.traced += size;
        if self.traced > self.peak {
            self.peak = self.traced;
        }
    }

    fn remove(&mut self, key: (u32, usize)) -> bool {
        match self.traces.remove(&key) {
            Some(t) => {
                self.traced -= t.size;
                true
            }
            None => false,
        }
    }

    fn clear(&mut self) {
        self.traces = HashMap::new();
        self.tracebacks = HashSet::new();
        self.filenames = HashSet::new();
        self.traced = 0;
        self.peak = 0;
    }
}

/// Raw view of the current thread's Python stack: (filename, lineno) of up
/// to `limit` frames, most recent first, and the total number of frames.
/// Frames that have not started executing yet are skipped.
fn capture(limit: usize, start: *mut crate::interp::frame::Frame) -> (Vec<(&'static str, i32)>, u32) {
    use crate::interp::frame::FrameState;
    let mut frames = Vec::new();
    let mut total = 0u32;
    let mut f = start;
    while !f.is_null() {
        let fr = unsafe { &*f };
        f = fr.prev;
        if fr.code.is_null() || fr.state == FrameState::Created || in_prologue(fr) {
            continue;
        }
        total = total.saturating_add(1);
        if frames.len() >= limit {
            continue;
        }
        let code = unsafe { &*(fr.code as *const crate::types::code::PyCodeObject) };
        let filename: &'static str = if code.co_filename.is_null() {
            UNKNOWN_FILENAME
        } else {
            let fo = unsafe { &*code.co_filename };
            match pystr::as_utf8(fo) {
                // The code object outlives this call; the str is only used
                // to build an interned copy below.
                Ok(s) => unsafe { std::mem::transmute::<&str, &'static str>(s) },
                Err(_) => {
                    clear_exc();
                    UNKNOWN_FILENAME
                }
            }
        };
        let lineno = fr.lineno().max(0);
        frames.push((filename, lineno));
    }
    if frames.is_empty() {
        frames.push((UNKNOWN_FILENAME, 0));
        total = 1;
    }
    (frames, total)
}

/// True while a frame is still running its prologue (creating cells,
/// copying free variables): such frames are not yet part of the
/// Python-visible stack.
fn in_prologue(fr: &crate::interp::frame::Frame) -> bool {
    use rpy_compiler::bytecode::{self as bc, Op};
    let data = crate::types::code::code_data(fr.code());
    match data.code.get(fr.lasti as usize) {
        Some(&word) => matches!(bc::decode(word).0, Op::MakeCell | Op::CopyFreeVars),
        None => false,
    }
}

fn record(domain: u32, ptr: usize, size: usize) {
    record_from(domain, ptr, size, crate::interp::current_frame_ptr());
}

fn record_from(domain: u32, ptr: usize, size: usize, start: *mut crate::interp::frame::Frame) {
    let limit = state().max_nframe;
    let (raw, total) = capture(limit, start);
    let mut st = state();
    let frames = raw.iter().map(|(name, line)| (st.intern_filename(name), *line)).collect();
    let tb = st.intern_traceback(Traceback { frames, total_nframe: total });
    st.add((domain, ptr), size, tb);
}

/// Allocator hook: a block of `size` bytes was allocated at `ptr`.
#[inline]
pub fn track(ptr: *const u8, size: usize) {
    if ptr.is_null() {
        return;
    }
    guarded(|| record(DEFAULT_DOMAIN, ptr as usize, size));
}

/// Allocator hook: the block at `ptr` is being freed.
#[inline]
pub fn untrack(ptr: *const u8) {
    if ptr.is_null() {
        return;
    }
    guarded(|| {
        state().remove((DEFAULT_DOMAIN, ptr as usize));
    });
}

/// Allocator hook: the block at `old` was resized/moved to `new`.
#[inline]
pub fn retrack(old: *const u8, new: *const u8, size: usize) {
    if new.is_null() {
        return;
    }
    guarded(|| {
        let mut st = state();
        let key = (DEFAULT_DOMAIN, old as usize);
        match st.traces.remove(&key) {
            Some(t) => {
                st.traced -= t.size;
                let tb = t.traceback;
                st.add((DEFAULT_DOMAIN, new as usize), size, tb);
            }
            None => {
                drop(st);
                record(DEFAULT_DOMAIN, new as usize, size);
            }
        }
    });
}

/// Start tracing with tracebacks of at most `nframe` frames.
pub fn start(nframe: i64) -> PyResult<()> {
    if !(1..=MAX_NFRAME).contains(&nframe) {
        raise!(ValueError, "the number of frames must be in range [1; {}]", MAX_NFRAME);
    }
    state().max_nframe = nframe as usize;
    TRACING.store(true, SeqCst);
    Ok(())
}

pub fn stop() {
    TRACING.store(false, SeqCst);
    guarded(|| state().clear());
}

fn frames_tuple(tb: &Traceback) -> PyRef {
    tuple::new_tuple(
        tb.frames
            .iter()
            .map(|(name, line)| tuple::new_tuple(vec![pystr::new_str(name), int::new_int(*line as i64)]))
            .collect(),
    )
}

/// The traceback (tuple of (filename, lineno), most recent first) of the
/// block at `ptr` in `domain`, or None.
pub fn get_traceback(domain: u32, ptr: usize) -> PyResult {
    if !TRACING.load(SeqCst) {
        return Ok(none::none_ref());
    }
    let tb = state().traces.get(&(domain, ptr)).map(|t| t.traceback.clone());
    Ok(match tb {
        Some(tb) => frames_tuple(&tb),
        None => none::none_ref(),
    })
}

// ------------------------------------------------------------ C API

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTraceMalloc_Track(domain: std::ffi::c_uint, ptr: usize, size: usize) -> std::ffi::c_int {
    if !TRACING.load(SeqCst) {
        return -2;
    }
    // an extension may call this with its thread state detached: the traceback is
    // still that of the calling thread
    let mut start = crate::interp::current_frame_ptr();
    if start.is_null() {
        start = crate::capi::pystate::detached_own_frame();
    }
    guarded(|| record_from(domain, ptr, size, start));
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTraceMalloc_Untrack(domain: std::ffi::c_uint, ptr: usize) -> std::ffi::c_int {
    if !TRACING.load(SeqCst) {
        return -2;
    }
    guarded(|| {
        state().remove((domain, ptr));
    });
    0
}

// ------------------------------------------------------------ module functions

fn tm_is_tracing(_m: &PyObject) -> PyResult {
    Ok(crate::types::bool_::new_bool(TRACING.load(SeqCst)))
}

fn tm_clear_traces(_m: &PyObject) -> PyResult {
    if TRACING.load(SeqCst) {
        guarded(|| state().clear());
    }
    Ok(fc::none())
}

fn tm_start(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("start", &["nframe"], 1, 0);
    let [n] = crate::args::parse::<1>(&P, args, kw)?;
    let nframe = match n {
        Some(v) => crate::abstract_::as_i64(v)?,
        None => 1,
    };
    start(nframe)?;
    Ok(fc::none())
}

fn tm_stop(_m: &PyObject) -> PyResult {
    stop();
    Ok(fc::none())
}

fn tm_get_traceback_limit(_m: &PyObject) -> PyResult {
    Ok(int::new_int(state().max_nframe as i64))
}

fn tm_get_tracemalloc_memory(_m: &PyObject) -> PyResult {
    let st = state();
    let mut size = std::mem::size_of::<State>();
    size += st.traces.capacity() * (std::mem::size_of::<(u32, usize)>() + std::mem::size_of::<Trace>());
    for tb in st.tracebacks.iter() {
        size += std::mem::size_of::<Traceback>() + tb.frames.capacity() * std::mem::size_of::<(Arc<str>, i32)>();
    }
    for name in st.filenames.iter() {
        size += name.len() + 16;
    }
    Ok(int::new_int(size as i64))
}

fn tm_get_traced_memory(_m: &PyObject) -> PyResult {
    let (cur, peak) = {
        let st = state();
        (st.traced, st.peak)
    };
    Ok(tuple::new_tuple(vec![int::new_int(cur as i64), int::new_int(peak as i64)]))
}

fn tm_reset_peak(_m: &PyObject) -> PyResult {
    let mut st = state();
    st.peak = st.traced;
    Ok(fc::none())
}

fn tm_get_traces(_m: &PyObject) -> PyResult {
    if !TRACING.load(SeqCst) {
        return Ok(list::new_list(Vec::new()));
    }
    // Copy out under the lock; build Python objects afterwards.
    let snapshot: Vec<(u32, usize, Arc<Traceback>)> = {
        let st = state();
        st.traces.iter().map(|(&(domain, _), t)| (domain, t.size, t.traceback.clone())).collect()
    };
    // Identical tracebacks share one tuple object.
    let mut converted: HashMap<*const Traceback, PyRef> = HashMap::new();
    let mut items = Vec::with_capacity(snapshot.len());
    for (domain, size, tb) in snapshot.iter() {
        let frames = converted.entry(Arc::as_ptr(tb)).or_insert_with(|| frames_tuple(tb)).clone();
        items.push(tuple::new_tuple(vec![
            int::new_int(*domain as i64),
            int::new_int(*size as i64),
            frames,
            int::new_int(tb.total_nframe as i64),
        ]));
    }
    Ok(list::new_list(items))
}

fn tm_get_object_traceback(_m: &PyObject, obj: &PyObject) -> PyResult {
    get_traceback(DEFAULT_DOMAIN, obj.as_ptr() as usize)
}

pub fn init_module() -> PyResult {
    let methods = mdefs![
        ("is_tracing", NOARGS, tm_is_tracing, "is_tracing($module, /)\n--\n\nReturn True if the tracemalloc module is tracing Python memory allocations."),
        ("clear_traces", NOARGS, tm_clear_traces, "clear_traces($module, /)\n--\n\nClear traces of memory blocks allocated by Python."),
        ("_get_traces", NOARGS, tm_get_traces, "_get_traces($module, /)\n--\n\nReturn a list of traces of memory blocks allocated by Python.\n\nEach trace is a tuple: (domain: int, size: int, traceback: tuple, total_nframe: int)."),
        ("_get_object_traceback", O, tm_get_object_traceback, "_get_object_traceback($module, obj, /)\n--\n\nGet the traceback where the Python object obj was allocated.\n\nReturn a tuple of (filename: str, lineno: int) tuples, or None."),
        ("start", FASTKW, tm_start, "start($module, nframe=1, /)\n--\n\nStart tracing Python memory allocations."),
        ("stop", NOARGS, tm_stop, "stop($module, /)\n--\n\nStop tracing Python memory allocations and clear traces."),
        ("get_traceback_limit", NOARGS, tm_get_traceback_limit, "get_traceback_limit($module, /)\n--\n\nGet the maximum number of frames stored in a traceback."),
        ("get_tracemalloc_memory", NOARGS, tm_get_tracemalloc_memory, "get_tracemalloc_memory($module, /)\n--\n\nGet the memory usage in bytes of the tracemalloc module."),
        ("get_traced_memory", NOARGS, tm_get_traced_memory, "get_traced_memory($module, /)\n--\n\nGet the current size and peak size of memory blocks traced by tracemalloc.\n\nReturns a tuple: (current: int, peak: int)."),
        ("reset_peak", NOARGS, tm_reset_peak, "reset_peak($module, /)\n--\n\nSet the peak size of memory blocks traced by tracemalloc to the current size."),
    ];
    let (m, _d) = fc::new_module("_tracemalloc", methods, Some("Debug module to trace memory blocks allocated by Python."))?;
    Ok(m)
}
