//! Bridge between Rust AST and the Python `ast` module (implemented in `_ast`).
use crate::errors::*;
use crate::ffi::PyObject;
pub fn is_ast_node(o: &PyObject) -> bool {
    crate::modules::_ast::is_ast_node(o)
}
pub fn compile_with_ast(src: &PyObject, fname: &str, mode: crate::compile::Mode, flags: u32, optimize: i32, feature_version: Option<u32>) -> PyResult {
    crate::modules::_ast::compile_with_ast(src, fname, mode, flags, optimize, feature_version)
}
