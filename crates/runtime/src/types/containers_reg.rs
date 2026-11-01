//! Registration of the "containers" workstream types (one hook each for
//! `types::init_all`, `types::builtin_types` and readiness), plus the
//! `__class_getitem__` helper shared by the container types.
use crate::errors::*;
use crate::ffi::{PyObject, PyTypeObject};
use crate::types::*;

/// `cls[item]` for builtin generic containers (PEP 585).
pub fn generic_alias(cls: &PyObject, item: &PyObject) -> PyResult {
    genericalias::new_generic_alias(cls, item)
}

/// Fill the static type objects (before any type_ready).
pub fn init_types() {
    tuple::init_types();
    list::init_types();
    dict_methods::init_types();
    dictview::init_types();
    frozendict::init_types();
    set::init_types();
    slice::init_types();
    mappingproxy::init_types();
    iterobject::init_types();
    range::init_types();
    enumerate::init_types();
    builtin_iters::init_types();
    property::init_types();
    weakref::init_types();
}

/// Ready the types not readied by the generic list in `types::init_all`.
pub fn ready() -> PyResult<()> {
    use typeobject::type_ready;
    tuple::ready()?;
    list::ready()?;
    dictview::ready()?;
    type_ready(frozendict::frozendict_type())?;
    set::ready()?;
    type_ready(mappingproxy::mappingproxy_type())?;
    iterobject::ready()?;
    range::ready()?;
    enumerate::ready()?;
    builtin_iters::ready()?;
    property::ready()?;
    weakref::ready()?;
    Ok(())
}

/// Builtin names provided by this workstream.
pub fn builtin_types() -> Vec<(&'static str, *mut PyTypeObject)> {
    vec![
        ("frozendict", frozendict::frozendict_type()),
        ("range", range::range_type()),
        ("enumerate", enumerate::enumerate_type()),
        ("reversed", enumerate::reversed_type()),
        ("zip", builtin_iters::zip_type()),
        ("map", builtin_iters::map_type()),
        ("filter", builtin_iters::filter_type()),
        ("property", property::property_type()),
    ]
}
