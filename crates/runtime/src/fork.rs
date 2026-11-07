//! fork() support for a runtime without a GIL.
//!
//! In a multi-threaded process only the forking thread survives in the
//! child, so any runtime lock another thread held at that moment would stay
//! locked forever.  `prepare()` therefore
//!   1. takes the import lock (Python code may hold it while it runs, so this
//!      must happen before the world is stopped),
//!   2. stops every other thread at a safepoint (gc::fork_prepare), which
//!      guarantees none of them is inside the allocator, the collector or the
//!      thread registry,
//!   3. takes the runtime's global locks itself.
//! After fork() both processes release those locks; the child additionally
//! retires the thread states of the threads that did not survive and resets
//! the collector.  Python-level hooks (os.register_at_fork) run around this
//! in posix::proc.

use std::sync::MutexGuard;

pub struct ForkState {
    stopped: bool,
    import_lock: bool,
    depot: Option<crate::tlab::DepotForkGuard>,
    all_threads: Option<MutexGuard<'static, Vec<usize>>>,
    free_states: Option<MutexGuard<'static, Vec<usize>>>,
    /// Number of other live Python threads when fork() was called.
    pub other_threads: usize,
}

/// Get the runtime ready for fork(); call `parent()` or `child()` after.
pub fn prepare() -> ForkState {
    let import_lock = crate::modules::_imp::acquire_import_lock().is_ok();
    let other_threads = count_other_threads();
    let stopped = crate::gc::fork_prepare();
    let depot = Some(crate::tlab::lock_depot_for_fork());
    let all_threads = Some(crate::threads::ALL_THREADS.lock().unwrap_or_else(|e| e.into_inner()));
    let free_states = Some(crate::threads::FREE_STATES.lock().unwrap_or_else(|e| e.into_inner()));
    ForkState { stopped, import_lock, depot, all_threads, free_states, other_threads }
}

fn release_locks(st: &mut ForkState) {
    drop(st.free_states.take());
    drop(st.all_threads.take());
    drop(st.depot.take());
}

/// In the parent after fork() (also when fork() failed).
pub fn parent(mut st: ForkState) {
    release_locks(&mut st);
    crate::gc::fork_parent(st.stopped);
    if st.import_lock {
        let _ = crate::modules::_imp::release_import_lock();
    }
}

/// In the child after fork().
pub fn child(mut st: ForkState) {
    release_locks(&mut st);
    crate::gc::fork_child();
    // Only this thread exists now.
    crate::threads::after_fork_child();
    crate::objlock::after_fork_child();
    let me = crate::threads::current_ptr();
    if !me.is_null() {
        crate::threads::MAIN_THREAD_STATE.store(me, std::sync::atomic::Ordering::Release);
        crate::threads::MAIN_THREAD_ID.store(unsafe { libc::pthread_self() } as u64, std::sync::atomic::Ordering::Release);
    }
    if st.import_lock {
        let _ = crate::modules::_imp::release_import_lock();
    }
    crate::modules::_imp::after_fork_child();
}

/// Live Python threads other than the caller.
pub fn count_other_threads() -> usize {
    let me = crate::threads::current_ptr() as usize;
    crate::threads::live_threads()
        .iter()
        .filter(|t| (**t as *const crate::threads::ThreadState as usize) != me && t.alive())
        .count()
}

/// The DeprecationWarning CPython emits in the parent when a process with
/// several threads forks.
pub fn warn_about_threads(other_threads: usize, func: &str) -> crate::errors::PyResult<()> {
    if other_threads == 0 {
        return Ok(());
    }
    let msg = format!(
        "This process (pid={}) is multi-threaded, use of {}() may lead to deadlocks in the child.",
        unsafe { libc::getpid() },
        func
    );
    crate::modules::warnings::warn(crate::types::exceptions::DeprecationWarning(), &msg, 1)
}
