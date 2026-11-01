//! Python-level codec registry lookup (codecs module), used for non-builtin encodings.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::types::str as pystr;
use crate::raise;
pub fn encode(s: &PyObject, enc: &str, errors: &str) -> PyResult {
    match crate::import::get_loaded_module("codecs").or_else(|| crate::import::import_absolute("codecs").ok()) {
        Some(c) => ab::call_method_str(&c, "encode", &[s, &pystr::new_str(enc), &pystr::new_str(errors)]),
        None => raise!(LookupError, "unknown encoding: {}", enc),
    }
}
pub fn decode(b: &PyObject, enc: &str, errors: &str) -> PyResult {
    match crate::import::get_loaded_module("codecs").or_else(|| crate::import::import_absolute("codecs").ok()) {
        Some(c) => ab::call_method_str(&c, "decode", &[b, &pystr::new_str(enc), &pystr::new_str(errors)]),
        None => raise!(LookupError, "unknown encoding: {}", enc),
    }
}
