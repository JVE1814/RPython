//! Bridge between Rust AST and the Python `ast` module (pending).
use crate::errors::*;
use crate::ffi::PyObject;
use crate::raise;
pub fn is_ast_node(o: &PyObject) -> bool {
    let _ = o;
    false
}
pub fn compile_with_ast(src: &PyObject, fname: &str, mode: crate::compile::Mode, flags: u32, optimize: i32) -> PyResult {
    let _ = (src, fname, mode, flags, optimize);
    raise!(NotImplementedError, "ast compilation not implemented yet");
}
