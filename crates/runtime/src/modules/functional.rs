//! Registration of the builtin modules owned by the "functional" workstream.

pub fn register() {
    use crate::import::register_builtin;
    register_builtin("_operator", super::_operator::init_module);
}
