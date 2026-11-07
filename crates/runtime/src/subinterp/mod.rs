//! Several interpreters in one process.
//!
//! An interpreter is the unit of isolation for Python-level state: its own `sys`
//! (and therefore `sys.modules`, `sys.path`, the import machinery), `builtins`,
//! `__main__`, warning filters, codec registry, atexit callbacks and module
//! objects of builtin modules.  Everything below the Python level is shared by
//! all interpreters of the process: the object heap and allocator, the cycle
//! collector and its safepoints, static (and immortal) objects, the thread
//! registry, compiled code caches and the JIT.  See notes/interp1.md (D26).
//!
//! Finding the interpreter: every thread state carries a pointer to its
//! interpreter (`PyThreadState.interp`, null meaning the main interpreter).
//! While no second interpreter was ever created, `current()` does not look at the
//! thread state at all (one relaxed load of `USED` and a predictable branch), so
//! single-interpreter programs pay nothing for this.
//!
//! Running code in another interpreter on the same OS thread works like a JVM
//! thread entering a different protection domain: the caller's thread state is
//! detached (it counts as "in native" for stop-the-world purposes), a thread
//! state of the target interpreter is attached, and the caller's state is
//! re-attached afterwards.  Objects never move between interpreters except as
//! copies produced by the cross-interpreter data layer (`xidata`).

pub mod xidata;
pub mod module;
pub mod queues;
pub mod channels;

use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::threads::{self, ThreadMode, ThreadState};
use crate::types::{dict, list, module as pymodule, none, str as pystr};
use std::any::Any;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicPtr, AtomicUsize, Ordering::*};
use std::sync::Mutex;

// ------------------------------------------------------------------ configuration

/// PyInterpreterConfig, as booleans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub use_main_obmalloc: bool,
    pub allow_fork: bool,
    pub allow_exec: bool,
    pub allow_threads: bool,
    pub allow_daemon_threads: bool,
    pub check_multi_interp_extensions: bool,
    /// 0 default, 1 shared, 2 own (there is no GIL; only reported back).
    pub gil: i32,
}

impl Config {
    /// The configuration `_interpreters.new_config('isolated')` describes.
    pub const ISOLATED: Config = Config {
        use_main_obmalloc: false,
        allow_fork: false,
        allow_exec: false,
        allow_threads: true,
        allow_daemon_threads: false,
        check_multi_interp_extensions: true,
        gil: 2,
    };
    /// The "legacy" configuration (Py_NewInterpreter).  Single-phase extension
    /// modules are still refused: this build is free-threaded.
    pub const LEGACY: Config = Config {
        use_main_obmalloc: true,
        allow_fork: true,
        allow_exec: true,
        allow_threads: true,
        allow_daemon_threads: true,
        check_multi_interp_extensions: true,
        gil: 1,
    };
    /// What the main interpreter reports.
    pub const MAIN: Config = Config {
        use_main_obmalloc: true,
        allow_fork: true,
        allow_exec: true,
        allow_threads: true,
        allow_daemon_threads: true,
        check_multi_interp_extensions: false,
        gil: 2,
    };
    /// Reject configurations a subinterpreter cannot be created with.  Without
    /// a GIL, single-phase extension modules can never be shared, so the check
    /// must stay enabled.
    pub fn validate(&self) -> Result<(), &'static std::ffi::CStr> {
        if !self.check_multi_interp_extensions {
            if !self.use_main_obmalloc {
                return Err(c"per-interpreter obmalloc does not support single-phase init extension modules");
            }
            return Err(c"the free-threaded build does not support single-phase init extension modules in subinterpreters");
        }
        if !(0..=2).contains(&self.gil) {
            return Err(c"invalid interpreter config 'gil' value");
        }
        Ok(())
    }
}

pub const WHENCE_UNKNOWN: i32 = 0;
pub const WHENCE_RUNTIME: i32 = 1;
pub const WHENCE_LEGACY_CAPI: i32 = 2;
pub const WHENCE_CAPI: i32 = 3;
pub const WHENCE_XI: i32 = 4;
pub const WHENCE_STDLIB: i32 = 5;

// ------------------------------------------------------------------ interpreter state

/// One interpreter.  The memory of an `Interp` is never reused (C code may keep a
/// `PyInterpreterState *` past Py_EndInterpreter); `alive` tells whether it ended.
pub struct Interp {
    // roots looked up on hot paths come first
    pub sys: AtomicPtr<PyObject>,
    pub modules: AtomicPtr<PyObject>,
    pub builtins: AtomicPtr<PyObject>,
    /// builtins.__import__ of this interpreter (identity check on IMPORT_NAME).
    pub import_fn: AtomicPtr<PyObject>,
    /// importlib._bootstrap once installed.
    pub bootstrap: AtomicPtr<PyObject>,
    pub importlib_ready: AtomicBool,
    pub id: i64,
    pub whence: AtomicI32,
    pub config: Config,
    pub alive: AtomicBool,
    /// Initialisation finished (list_all(require_ready=True)).
    pub ready: AtomicBool,
    pub finalizing: AtomicBool,
    /// PyInterpreterState_GetDict (owned, 0 until first use).
    pub dict: Mutex<usize>,
    /// PyUnstable_AtExit callbacks (function, data).
    pub c_atexit: Mutex<Vec<(usize, usize)>>,
    /// Builtin module objects created in this interpreter (see import.rs).
    pub builtin_objects: Mutex<Vec<(String, usize)>>,
    /// Lazily created per-interpreter values of `PerInterp` statics, keyed by the
    /// static's address.
    ext: Mutex<Vec<(usize, &'static (dyn Any + Send + Sync))>>,
    /// Thread state currently running code in `__main__` on behalf of
    /// `_interpreters.exec` & co. (0 = idle).
    pub running: AtomicUsize,
    /// Number of threads currently executing in this interpreter (entered and
    /// not yet left), the main-code runner included.
    pub active: AtomicUsize,
    /// `_interpreters.incref`/`decref` count and whether the interpreter is
    /// destroyed when it drops to zero.
    pub refs: AtomicI64,
    pub requires_idref: AtomicBool,
    /// `_imp._override_multi_interp_extensions_check`: -1 off, 0 config, 1 on.
    pub override_ext_check: AtomicI32,
}

unsafe impl Sync for Interp {}
unsafe impl Send for Interp {}

impl Interp {
    const fn new(id: i64, whence: i32, config: Config) -> Interp {
        Interp {
            sys: AtomicPtr::new(std::ptr::null_mut()),
            modules: AtomicPtr::new(std::ptr::null_mut()),
            builtins: AtomicPtr::new(std::ptr::null_mut()),
            import_fn: AtomicPtr::new(std::ptr::null_mut()),
            bootstrap: AtomicPtr::new(std::ptr::null_mut()),
            importlib_ready: AtomicBool::new(false),
            id,
            whence: AtomicI32::new(whence),
            config,
            alive: AtomicBool::new(true),
            ready: AtomicBool::new(false),
            finalizing: AtomicBool::new(false),
            dict: Mutex::new(0),
            c_atexit: Mutex::new(Vec::new()),
            builtin_objects: Mutex::new(Vec::new()),
            ext: Mutex::new(Vec::new()),
            running: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            refs: AtomicI64::new(0),
            requires_idref: AtomicBool::new(false),
            override_ext_check: AtomicI32::new(0),
        }
    }
    #[inline]
    pub fn is_main(&self) -> bool {
        std::ptr::eq(self, &MAIN)
    }
    pub fn as_ptr(&self) -> *mut c_void {
        self as *const Interp as *mut c_void
    }
    /// Should importing a single-phase extension module fail here?
    pub fn checks_extensions(&self) -> bool {
        match self.override_ext_check.load(Relaxed) {
            o if o > 0 => true,
            o if o < 0 => false,
            _ => self.config.check_multi_interp_extensions,
        }
    }
    pub fn is_running(&self) -> bool {
        self.is_main() || self.running.load(Acquire) != 0
    }
}

pub static MAIN: Interp = Interp::new(0, WHENCE_RUNTIME, Config::MAIN);

/// Set (for good) when the first subinterpreter is created.
pub static USED: AtomicBool = AtomicBool::new(false);
static LIVE: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

/// The interpreter of the calling thread.
#[inline(always)]
pub fn current() -> &'static Interp {
    if !USED.load(Relaxed) {
        return &MAIN;
    }
    current_slow()
}

#[inline(never)]
fn current_slow() -> &'static Interp {
    of_ts(threads::current_ptr())
}

/// The interpreter a thread state belongs to (null state: the main interpreter).
pub fn of_ts(ts: *const ThreadState) -> &'static Interp {
    if ts.is_null() {
        return &MAIN;
    }
    let p = unsafe { (*ts).c.interp };
    if p.is_null() { &MAIN } else { unsafe { &*(p as *const Interp) } }
}

/// `PyInterpreterState *` -> interpreter (null: main).
pub unsafe fn from_ptr(p: *mut c_void) -> &'static Interp {
    if p.is_null() { &MAIN } else { unsafe { &*(p as *const Interp) } }
}

pub fn main() -> &'static Interp {
    &MAIN
}

/// Live interpreters, main first, then in creation order.
pub fn list_all() -> Vec<&'static Interp> {
    let mut v = vec![&MAIN];
    for &p in LIVE.lock().unwrap().iter() {
        v.push(unsafe { &*(p as *const Interp) });
    }
    v
}

pub fn lookup(id: i64) -> Option<&'static Interp> {
    if id == 0 {
        return Some(&MAIN);
    }
    LIVE.lock().unwrap().iter().map(|&p| unsafe { &*(p as *const Interp) }).find(|i| i.id == id)
}

/// The id the next interpreter will get.
pub fn next_id() -> i64 {
    NEXT_ID.load(Relaxed)
}

pub fn is_live(i: &Interp) -> bool {
    i.is_main() || LIVE.lock().unwrap().contains(&(i as *const Interp as usize))
}

// ------------------------------------------------------------------ per-interpreter statics

/// A static with one value per interpreter.  The main interpreter's value lives
/// in the static itself; other interpreters get theirs on first use, and it is
/// dropped when the interpreter is destroyed (with that interpreter attached, so
/// Python references inside it are released in the right place).
pub struct PerInterp<T: Send + Sync + 'static> {
    main: std::sync::LazyLock<T, fn() -> T>,
    init: fn() -> T,
}

impl<T: Send + Sync + 'static> PerInterp<T> {
    pub const fn new(init: fn() -> T) -> Self {
        PerInterp { main: std::sync::LazyLock::new(init), init }
    }
    #[inline]
    pub fn get(&'static self) -> &'static T {
        if !USED.load(Relaxed) {
            return &self.main;
        }
        self.get_in(current())
    }
    pub fn get_in(&'static self, i: &'static Interp) -> &'static T {
        if i.is_main() {
            return &self.main;
        }
        let key = self as *const Self as usize;
        let mut ext = i.ext.lock().unwrap();
        if let Some((_, v)) = ext.iter().find(|(k, _)| *k == key) {
            return v.downcast_ref::<T>().unwrap();
        }
        drop(ext);
        // the initialiser may itself touch per-interpreter state
        let fresh: &'static T = Box::leak(Box::new((self.init)()));
        ext = i.ext.lock().unwrap();
        if let Some((_, v)) = ext.iter().find(|(k, _)| *k == key) {
            return v.downcast_ref::<T>().unwrap();
        }
        ext.push((key, fresh));
        fresh
    }
}

// ------------------------------------------------------------------ thread states

fn detach_current() -> *mut ThreadState {
    let p = threads::current_ptr();
    if !p.is_null() {
        unsafe { (*p).mode.set(ThreadMode::InNative) };
    }
    threads::set_current(std::ptr::null_mut());
    p
}

fn attach(ts: *mut ThreadState) {
    threads::set_current(ts);
    if !ts.is_null() {
        unsafe {
            (*ts).mode.set(ThreadMode::InVm);
            crate::gc::safepoint_poll(&*ts);
        }
    }
}

/// A new (detached) thread state of `i` for the calling OS thread.
pub fn new_thread_state(i: &Interp) -> *mut ThreadState {
    let ts = threads::new_thread_state();
    unsafe {
        (*ts).c.interp = if i.is_main() { std::ptr::null_mut() } else { i.as_ptr() };
        (*ts).mode.set(ThreadMode::InNative);
    }
    ts
}

/// Retire a thread state that is not attached while its OS thread lives on
/// (the per-OS-thread parts of `threads::thread_exit` must not run).
pub unsafe fn retire_thread_state(p: *mut ThreadState) {
    unsafe {
        let ts = &*p;
        threads::process_merge_queue(ts);
        let e = std::mem::replace(&mut (*p).c.current_exception, std::ptr::null_mut());
        xdecref(e);
        let a = ts.async_exc.swap(std::ptr::null_mut(), AcqRel);
        xdecref(a);
        for cellp in [&ts.trace_func, &ts.profile_func] {
            let f = cellp.replace(std::ptr::null_mut());
            xdecref(f);
        }
        for slot in [&raw mut (*p).c.async_gen_firstiter, &raw mut (*p).c.async_gen_finalizer, &raw mut (*p).c.context, &raw mut (*p).c.dict] {
            let o = std::mem::replace(&mut *slot, std::ptr::null_mut());
            xdecref(o);
        }
        {
            let _all = threads::ALL_THREADS.lock().unwrap_or_else(|e| e.into_inner());
            ts.alive.store(false, SeqCst);
        }
        loop {
            let rest: Vec<usize> = std::mem::take(&mut *ts.merge_queue.lock().unwrap_or_else(|e| e.into_inner()));
            if rest.is_empty() {
                break;
            }
            for op in rest {
                merge_queued(op as *mut PyObject);
            }
        }
        ts.c.eval_breaker.fetch_and(!threads::EVAL_MERGE_REFCOUNTS, AcqRel);
        ts.mode.set(ThreadMode::Blocked);
        ts.exited.store(true, SeqCst);
        {
            let mut all = threads::ALL_THREADS.lock().unwrap_or_else(|e| e.into_inner());
            all.retain(|&t| t != p as usize);
            crate::qsbr::note_threads(all.len());
        }
        threads::FREE_STATES.lock().unwrap_or_else(|e| e.into_inner()).push(p as usize);
    }
}

/// The calling thread runs in another interpreter until `leave`.
pub struct Entered {
    prev: *mut ThreadState,
    ts: *mut ThreadState,
    interp: &'static Interp,
}

/// Attach a fresh thread state of `i` to the calling OS thread (the caller's
/// state, if any, is detached and comes back with `leave`).
pub fn enter(i: &'static Interp) -> Entered {
    let prev = detach_current();
    let ts = new_thread_state(i);
    i.active.fetch_add(1, AcqRel);
    attach(ts);
    Entered { prev, ts, interp: i }
}

impl Entered {
    pub fn thread_state(&self) -> *mut ThreadState {
        self.ts
    }
    pub fn interp(&self) -> &'static Interp {
        self.interp
    }
}

/// Undo `enter`: the temporary thread state is retired (a pending exception in
/// it is dropped: callers capture what they need first).
pub fn leave(e: Entered) {
    unsafe {
        let cur = threads::current_ptr();
        debug_assert!(cur == e.ts);
        let _ = detach_current();
        if cur != e.ts && !cur.is_null() {
            // C code swapped thread states and did not restore them
            threads::set_current(std::ptr::null_mut());
        }
        retire_thread_state(e.ts);
        e.interp.active.fetch_sub(1, AcqRel);
        attach(e.prev);
    }
}

/// Run `f` with the calling thread in interpreter `i`.
pub fn with_interp<R>(i: &'static Interp, f: impl FnOnce() -> R) -> R {
    if std::ptr::eq(current(), i) {
        return f();
    }
    let e = enter(i);
    let r = f();
    leave(e);
    r
}

// ------------------------------------------------------------------ creation

/// sys.path entries a new interpreter starts with (the main interpreter's
/// configured search path, without the script directory).
static INITIAL_PATH: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Record the configured module search path (called once by `initialize`).
pub fn set_initial_path(p: Vec<String>) {
    *INITIAL_PATH.lock().unwrap() = p;
}

fn register(i: &'static Interp) {
    USED.store(true, SeqCst);
    LIVE.lock().unwrap().push(i as *const Interp as usize);
}

fn unregister(i: &Interp) {
    LIVE.lock().unwrap().retain(|&p| p != i as *const Interp as usize);
}

/// PyInterpreterState_New: an interpreter without any Python-level state.
pub fn new_uninitialized() -> &'static Interp {
    let id = NEXT_ID.fetch_add(1, Relaxed);
    let interp: &'static Interp = Box::leak(Box::new(Interp::new(id, WHENCE_UNKNOWN, Config::LEGACY)));
    register(interp);
    interp
}

/// Create a new interpreter.  On success its (initial) thread state is attached
/// to the calling thread and returned, together with the thread state that was
/// attached before (detached now).  On failure the previous state is attached
/// again and the error is set there.
pub fn create(config: Config, whence: i32) -> PyResult<(*mut ThreadState, *mut ThreadState)> {
    if let Err(msg) = config.validate() {
        return Err(new_err(module::interpreter_error_type(), &msg.to_string_lossy()));
    }
    let id = NEXT_ID.fetch_add(1, Relaxed);
    let interp: &'static Interp = Box::leak(Box::new(Interp::new(id, whence, config)));
    register(interp);
    // the new interpreter starts with the argv of the main one
    let argv: Vec<String> = with_interp(&MAIN, || {
        crate::modules::sys::get_sys_attr("argv")
            .filter(|a| list::is_list(a))
            .map(|a| list::list_to_vec(&a).iter().map(|s| if pystr::is_str(s) { pystr::to_string_lossy(s) } else { String::new() }).collect())
            .unwrap_or_default()
    });
    let prev = detach_current();
    let ts = new_thread_state(interp);
    interp.active.fetch_add(1, AcqRel);
    attach(ts);
    match init_current(&argv) {
        Ok(()) => {
            interp.ready.store(true, Release);
            Ok((ts, prev))
        }
        Err(_) => {
            let msg = describe_pending();
            finalize_current();
            interp.alive.store(false, Release);
            unregister(interp);
            let _ = detach_current();
            unsafe { retire_thread_state(ts) };
            interp.active.fetch_sub(1, AcqRel);
            attach(prev);
            Err(crate::errors::new_err(
                module::interpreter_error_type(),
                &format!("interpreter creation failed: {}", msg),
            ))
        }
    }
}

/// "Type: message" of the pending exception (cleared).
fn describe_pending() -> String {
    match take_exc() {
        Some(e) => {
            let t = crate::types::typeobject::tname(&e);
            match crate::abstract_::str_string(&e) {
                Ok(s) if !s.is_empty() => format!("{}: {}", t, s),
                _ => {
                    clear_exc();
                    t.to_string()
                }
            }
        }
        None => "unknown error".into(),
    }
}

/// Build the Python-level state of the (attached, empty) current interpreter.
fn init_current(argv: &[String]) -> PyResult<()> {
    let b = crate::builtins::init()?;
    let sys = crate::modules::sys::create_sys_module(argv)?;
    let md = crate::modules::sys::modules_dict();
    dict::set_item_str(&md, "builtins", &b)?;
    let path = crate::modules::sys::get_sys_attr("path").unwrap();
    // the main interpreter's script directory (or cwd) is importable here too
    if let Some(p0) = crate::import::sys_path_0() {
        list::append(&path, pystr::new_str(&p0));
    }
    for p in INITIAL_PATH.lock().unwrap().clone() {
        list::append(&path, pystr::new_str(&p));
    }
    crate::import::install_importlib(&crate::modules::sys_full::stdlib_dir())?;
    crate::modules::sys::init_stdio();
    if !crate::modules::sys_full::config().warnoptions.is_empty() && crate::import::import_absolute("warnings").is_err() {
        clear_exc();
    }
    crate::modules::sys_full::import_site()?;
    let main = pymodule::new_module("__main__")?;
    let d = pymodule::module_dict(&main);
    dict::set_item_str(&d, "__builtins__", &b)?;
    if let Some(bs) = crate::import::bootstrap() {
        if let Ok(l) = crate::abstract_::getattr_str(&bs, "BuiltinImporter") {
            dict::set_item_str(&d, "__loader__", &l)?;
        } else {
            clear_exc();
        }
    }
    dict::set_item_str(&md, "__main__", &main)?;
    drop(sys);
    Ok(())
}

// ------------------------------------------------------------------ finalization

/// Number of live thread states of `i` other than the calling one.
fn other_threads(i: &Interp) -> usize {
    let me = threads::current_ptr() as usize;
    threads::ALL_THREADS
        .lock()
        .unwrap()
        .iter()
        .filter(|&&t| t != me && std::ptr::eq(of_ts(t as *const ThreadState), i))
        .filter(|&&t| unsafe { (*(t as *const ThreadState)).alive.load(Acquire) })
        .count()
}

/// Shut down the Python-level state of the current (sub)interpreter: wait for its
/// non-daemon threads, run its atexit callbacks, tear its modules down and drop
/// its roots.  The thread state stays attached.
pub fn finalize_current() {
    let i = current();
    if i.is_main() || i.finalizing.swap(true, AcqRel) {
        return;
    }
    let saved = take_exc();
    if !i.modules.load(Acquire).is_null() {
        crate::modules::sys_finalize::wait_for_thread_shutdown();
        crate::modules::atexit::run_exitfuncs();
        run_c_atexit(i);
        crate::modules::sys_full::flush_std("stdout");
        crate::modules::sys_full::flush_std("stderr");
        clear_exc();
        crate::gc::collect();
        clear_exc();
        crate::modules::sys_finalize::finalize_modules();
        clear_exc();
    } else {
        run_c_atexit(i);
    }
    // per-interpreter statics (codec registry, warning filters, ...)
    let ext: Vec<(usize, &'static (dyn Any + Send + Sync))> = std::mem::take(&mut *i.ext.lock().unwrap());
    let lingering = other_threads(i) > 0;
    if !lingering {
        for (_, v) in ext {
            // leaked by PerInterp::get_in; nobody else can reach it any more
            unsafe { drop(Box::from_raw(v as *const (dyn Any + Send + Sync) as *mut (dyn Any + Send + Sync))) };
        }
        let d = std::mem::replace(&mut *i.dict.lock().unwrap(), 0);
        unsafe { xdecref(d as *mut PyObject) };
        for (_, p) in std::mem::take(&mut *i.builtin_objects.lock().unwrap()) {
            unsafe { xdecref(p as *mut PyObject) };
        }
        // the namespaces of sys and builtins hold the rest of the object graph
        for root in [&i.sys, &i.builtins] {
            let p = root.load(Acquire);
            if !p.is_null() {
                let d = pymodule::module_dict(unsafe { &*p });
                dict::clear(&d);
            }
        }
        let md = i.modules.load(Acquire);
        if !md.is_null() {
            dict::clear(unsafe { &*md });
        }
        clear_exc();
        crate::gc::collect();
        clear_exc();
    } else {
        std::mem::forget(ext);
    }
    if let Some(e) = saved {
        set_exc(e);
    }
}

fn run_c_atexit(i: &Interp) {
    let v: Vec<(usize, usize)> = std::mem::take(&mut *i.c_atexit.lock().unwrap());
    for (f, d) in v.into_iter().rev() {
        let f: unsafe extern "C" fn(*mut c_void) = unsafe { std::mem::transmute(f) };
        unsafe { f(d as *mut c_void) };
    }
}

/// Mark an interpreter as gone (after `finalize_current`).
pub fn mark_destroyed(i: &'static Interp) {
    i.alive.store(false, Release);
    unregister(i);
    queues::interp_destroyed(i.id);
    channels::interp_destroyed(i.id);
}

/// After `create`: retire the new interpreter's initial thread state and
/// re-attach the caller's.
pub fn leave_created(ts: *mut ThreadState, prev: *mut ThreadState) {
    let i = of_ts(ts);
    let _ = detach_current();
    unsafe { retire_thread_state(ts) };
    i.active.fetch_sub(1, AcqRel);
    attach(prev);
}

/// Py_EndInterpreter: `ts` is the attached thread state of a subinterpreter.
/// Afterwards no thread state is attached.
pub unsafe fn end_interpreter(ts: *mut ThreadState) {
    let i = of_ts(ts);
    if i.is_main() {
        return;
    }
    finalize_current();
    mark_destroyed(i);
    let _ = detach_current();
    unsafe { retire_thread_state(ts) };
    i.active.fetch_sub(1, AcqRel);
}

/// Destroy interpreter `i` from another interpreter (`_interpreters.destroy`).
pub fn destroy(i: &'static Interp) -> PyResult<()> {
    if i.is_main() {
        return Err(new_err(module::interpreter_error_type(), "cannot destroy the main interpreter"));
    }
    if std::ptr::eq(current(), i) {
        return Err(new_err(module::interpreter_error_type(), "cannot destroy the current interpreter"));
    }
    if i.running.load(Acquire) != 0 {
        return Err(new_err(module::interpreter_error_type(), "interpreter running"));
    }
    if !is_live(i) {
        return Err(not_found(i.id));
    }
    // claim it so that nobody starts running code in it meanwhile
    if i.running.compare_exchange(0, usize::MAX, AcqRel, Acquire).is_err() {
        return Err(new_err(module::interpreter_error_type(), "interpreter running"));
    }
    let e = enter(i);
    finalize_current();
    leave(e);
    mark_destroyed(i);
    Ok(())
}

pub fn not_found(id: i64) -> PyErr {
    new_err(module::not_found_error_type(), &format!("unrecognized interpreter ID {}", id))
}

/// Destroy every subinterpreter that is still alive (main interpreter shutdown).
pub fn destroy_all() {
    let all: Vec<&'static Interp> = list_all().into_iter().filter(|i| !i.is_main()).collect();
    for i in all {
        if i.running.load(Acquire) == 0 && i.active.load(Acquire) == 0 && destroy(i).is_err() {
            clear_exc();
        }
    }
}

// ------------------------------------------------------------------ running __main__ code

/// Mark `i` as running main code on the calling thread; fails if it is already.
pub fn claim_running(i: &'static Interp) -> PyResult<()> {
    let me = threads::current_ptr() as usize | 1;
    if i.running.compare_exchange(0, me, AcqRel, Acquire).is_err() {
        return Err(new_err(module::interpreter_error_type(), "interpreter already running"));
    }
    Ok(())
}

pub fn release_running(i: &'static Interp) {
    i.running.store(0, Release);
}

/// The `__main__` module of the current interpreter (created if missing).
pub fn main_module() -> PyResult {
    let md = crate::modules::sys::modules_dict();
    if let Some(m) = dict::get_item_rs(&md, "__main__") {
        if !m.is(none::none()) {
            return Ok(m);
        }
    }
    let m = pymodule::new_module("__main__")?;
    let d = pymodule::module_dict(&m);
    dict::set_item_str(&d, "__builtins__", &crate::builtins::builtins_module())?;
    dict::set_item_str(&md, "__main__", &m)?;
    Ok(m)
}

/// The interpreter's per-interpreter config flags checked by os/threading.
pub fn current_config() -> Config {
    current().config
}

/// RuntimeError unless the current interpreter allows `what`.
pub fn check_allowed(allowed: bool, what: &str) -> PyResult<()> {
    if allowed {
        return Ok(());
    }
    Err(new_err(crate::types::exceptions::RuntimeError(), &format!("{} not supported for isolated subinterpreters", what)))
}
