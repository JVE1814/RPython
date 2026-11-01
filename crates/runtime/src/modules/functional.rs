//! Registration of the builtin modules owned by the "functional" workstream.

pub fn register() {
    use crate::import::register_builtin;
    register_builtin("_operator", super::_operator::init_module);
    register_builtin("_functools", super::_functools::init_module);
    register_builtin("_abc", super::_abc::init_module);
    register_builtin("itertools", super::itertools::init_module);
    register_builtin("_collections", super::_collections::init_module);
    register_builtin("_types", super::_types::init_module);
    register_builtin("_heapq", super::_heapq::init_module);
    register_builtin("_bisect", super::_bisect::init_module);
    register_builtin("atexit", super::atexit::init_module);
    register_builtin("_contextvars", super::_contextvars::init_module);
}
