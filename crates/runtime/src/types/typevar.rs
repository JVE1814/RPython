//! PEP 695 typing objects (TypeVar, ParamSpec, TypeVarTuple, TypeAliasType, Generic) — stub.
use crate::errors::*;
use crate::ffi::PyObject;
use crate::raise;
pub fn make_typevar(name: &PyObject, bound: Option<&PyObject>, constraints: Option<&PyObject>, infer: bool) -> PyResult {
    let _ = (name, bound, constraints, infer);
    raise!(NotImplementedError, "TypeVar not implemented yet");
}
pub fn make_paramspec(name: &PyObject) -> PyResult {
    let _ = name;
    raise!(NotImplementedError, "ParamSpec not implemented yet");
}
pub fn make_typevartuple(name: &PyObject) -> PyResult {
    let _ = name;
    raise!(NotImplementedError, "TypeVarTuple not implemented yet");
}
pub fn subscript_generic(params: &PyObject) -> PyResult {
    let _ = params;
    raise!(NotImplementedError, "Generic not implemented yet");
}
pub fn make_typealias(args: &PyObject) -> PyResult {
    let _ = args;
    raise!(NotImplementedError, "TypeAliasType not implemented yet");
}
pub fn set_default(tp: &PyObject, default: &PyObject) -> PyResult {
    let _ = (tp, default);
    raise!(NotImplementedError, "type parameter defaults not implemented yet");
}
