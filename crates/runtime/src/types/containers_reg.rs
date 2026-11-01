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
    unsafe {
        disallow_instantiation(&[
            &raw mut dictview::PyDictKeys_Type,
            &raw mut dictview::PyDictValues_Type,
            &raw mut dictview::PyDictItems_Type,
            &raw mut dictview::PyDictIterKey_Type,
            &raw mut dictview::PyDictIterValue_Type,
            &raw mut dictview::PyDictIterItem_Type,
            &raw mut dictview::PyDictRevIterKey_Type,
            &raw mut dictview::PyDictRevIterValue_Type,
            &raw mut dictview::PyDictRevIterItem_Type,
            &raw mut list::PyListIter_Type,
            &raw mut list::PyListRevIter_Type,
            &raw mut tuple::PyTupleIter_Type,
            &raw mut set::PySetIter_Type,
            &raw mut iterobject::PySeqIter_Type,
            &raw mut iterobject::PyCallIter_Type,
            &raw mut range::PyRangeIter_Type,
            &raw mut range::PyLongRangeIter_Type,
            &raw mut weakref::_PyWeakref_ProxyType,
            &raw mut weakref::_PyWeakref_CallableProxyType,
        ]);
    }
}

/// `tp_new` for types that cannot be instantiated from Python.
pub unsafe extern "C" fn no_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    let _ = crate::errors::new_err(
        crate::types::exceptions::TypeError(),
        &format!("cannot create '{}' instances", typeobject::type_fqname(tp)),
    );
    std::ptr::null_mut()
}

/// Mark static types as not instantiable from Python.
pub fn disallow_instantiation(types: &[*mut PyTypeObject]) {
    for &t in types {
        unsafe {
            (*t).tp_new = Some(no_new);
            (*t).tp_flags |= crate::ffi::Py_TPFLAGS_DISALLOW_INSTANTIATION;
        }
    }
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
