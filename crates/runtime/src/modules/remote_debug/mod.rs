//! (remote1) Remote debugging support (PEP 768 and the out-of-process profilers):
//! `_remote_debugging`, the debug table that describes this runtime's memory layout, the
//! process-memory reader, and the target-side task registry and `sys.remote_exec`
//! handler.

pub mod offsets;
pub mod procmem;
pub mod reader;
pub mod target;

pub use offsets::{EVAL_REMOTE_EXEC, REMOTE_EXEC, init_table, remote_exec_allowed};

// Thread status flags reported by RemoteUnwinder (shared with profiling.sampling).
pub const THREAD_STATUS_HAS_GIL: i64 = 1 << 0;
pub const THREAD_STATUS_ON_CPU: i64 = 1 << 1;
pub const THREAD_STATUS_UNKNOWN: i64 = 1 << 2;
pub const THREAD_STATUS_GIL_REQUESTED: i64 = 1 << 3;
pub const THREAD_STATUS_HAS_EXCEPTION: i64 = 1 << 4;
pub const THREAD_STATUS_MAIN_THREAD: i64 = 1 << 5;

/// Queue a `sys.remote_exec` script for the main thread (used only for self-targeting;
/// cross-process the debugger writes the mailbox itself).
pub fn queue_self_remote_exec(path: &[u8]) -> bool {
    let area = &REMOTE_EXEC;
    if path.len() >= offsets::REMOTE_PATH_MAX {
        return false;
    }
    unsafe {
        let buf = &mut *area.path.get();
        buf[..path.len()].copy_from_slice(path);
        buf[path.len()] = 0;
    }
    area.pending.store(1, std::sync::atomic::Ordering::Release);
    true
}
