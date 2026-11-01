pub mod annotations;
pub mod builtins_super;
pub mod copyreg_support;
pub mod signal;
pub mod suggestions;
pub mod sys;
pub mod thread;
pub mod traceback_util;
pub mod warnings;
pub mod _warnings;
pub mod sys_full;
pub mod sys_trace;
pub mod traceback_print;
pub mod ast_bridge;
pub mod os_fspath;
pub mod sys_trace_api;
pub mod sys_structseq;
pub mod sys_namespace;
pub mod _thread;
pub mod pymain;
pub mod codecs_core;
pub mod codecs_registry;
pub mod reprlib_guard;
pub mod bufview;
pub mod errno;
pub mod posix;
pub mod stat_mod;
pub mod time;
pub mod fncommon;
pub mod functional;
pub mod _operator;
pub mod _functools;
pub mod _abc;
pub mod itertools;

/// Register all builtin (Rust-implemented) modules with the import system.
pub fn register_all() {
pub mod builtins_numbers;
pub mod math;
pub mod numbers_registry;
    crate::import::register_builtin("posix", posix::init);
    crate::import::register_builtin("errno", errno::init);
    crate::import::register_builtin("_stat", stat_mod::init);
    crate::import::register_builtin("time", time::init);
    functional::register();
}
    numbers_registry::register();
    crate::import::register_builtin("_warnings", _warnings::init_module);
    sys_full::register_system_modules();
