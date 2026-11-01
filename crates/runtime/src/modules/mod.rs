pub mod annotations;
pub mod builtins_super;
pub mod copyreg_support;
pub mod signal;
pub mod suggestions;
pub mod sys;
pub mod thread;
pub mod traceback_util;
pub mod warnings;
pub mod sys_full;
pub mod sys_trace;
pub mod traceback_print;
pub mod ast_bridge;
pub mod os_fspath;
pub mod sys_trace_api;
pub mod codecs_core;
pub mod codecs_registry;
pub mod reprlib_guard;

pub mod _weakref;

/// Register all builtin (Rust-implemented) modules with the import system.
pub fn register_all() {
    crate::import::register_builtin("_weakref", _weakref::init);
}
