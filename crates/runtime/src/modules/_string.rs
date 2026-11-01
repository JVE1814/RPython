//! `_string`: helpers for `string.Formatter` (the parser itself lives with
//! str.format in types/str_methods/format.rs).

use crate::errors::*;
use crate::ffi::PyObject;
use crate::types::str_methods::format as fmt;

fn formatter_parser(_m: &PyObject, s: &PyObject) -> PyResult {
    fmt::formatter_parser(s)
}

fn formatter_field_name_split(_m: &PyObject, s: &PyObject) -> PyResult {
    fmt::formatter_field_name_split(s)
}

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("_string")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("formatter_parser", O, formatter_parser, "parse the argument as a format string"),
            ("formatter_field_name_split", O, formatter_field_name_split, "split the argument as a field name"),
        ],
        Some(&m),
    )?;
    crate::types::dict::set_item_str(&d, "__doc__", &crate::types::str::new_str("string helper module"))?;
    Ok(m)
}
