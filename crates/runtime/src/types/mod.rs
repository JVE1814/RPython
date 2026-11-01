pub mod asyncgen;
pub mod bool_;
pub mod bytearray;
pub mod bytes;
pub mod cfunction;
pub mod code;
pub mod complex;
pub mod descr;
pub mod dict;
pub mod dict_methods;
pub mod exceptiongroup;
pub mod exceptions;
pub mod float;
pub mod frameobject;
pub mod funcobject;
pub mod genobject;
pub mod int;
pub mod int_methods;
pub mod iterobject;
pub mod list;
pub mod list_methods;
pub mod mappingproxy;
pub mod memoryview;
pub mod module;
pub mod none;
pub mod object_type;
pub mod set;
pub mod slice;
pub mod slots;
pub mod str;
pub mod super_;
pub mod templatelib;
pub mod traceback;
pub mod tuple;
pub mod typeobject;
pub mod typevar;
pub mod union;
pub mod weakref;
pub mod frameobject_proxy;
pub mod format_impl;
pub mod core_slots;
pub mod dictview;
pub mod float_math;
pub mod float_methods;
pub mod complex_methods;
pub mod str_sub;
pub mod str_methods;

use crate::ffi::PyTypeObject;

/// (builtins name, type) for every builtin type exposed in the builtins module.
pub fn builtin_types() -> Vec<(&'static str, *mut PyTypeObject)> {
    vec![
        ("object", typeobject::object_type()),
        ("type", typeobject::type_type()),
        ("int", int::int_type()),
        ("bool", bool_::bool_type()),
        ("float", float::float_type()),
        ("complex", complex::complex_type()),
        ("str", str::str_type()),
        ("bytes", bytes::bytes_type()),
        ("bytearray", bytearray::bytearray_type()),
        ("tuple", tuple::tuple_type()),
        ("list", list::list_type()),
        ("dict", dict::dict_type()),
        ("set", set::set_type()),
        ("frozenset", set::frozenset_type()),
        ("slice", slice::slice_type()),
        ("staticmethod", funcobject::staticmethod_type()),
        ("classmethod", funcobject::classmethod_type()),
        ("super", super_::super_type()),
    ]
    .into_iter()
    .chain(bootstrap_builtins::types())
    .collect()
}

/// Fill all static type objects, then ready them.
pub fn init_all() -> crate::errors::PyResult<()> {
    object_type::init_types();
    descr::init_types();
    cfunction::init_types();
    funcobject::init_types();
    none::init_types();
    int_methods::init_types();
    bool_::init_types();
    float_methods::init_types();
    complex_methods::init_types();
    core_slots::init_types();
    exceptions::init_types();
    code::init_types();
    module::init_types();
    frameobject::init_types();
    traceback::init_types();
    genobject::init_types();
    iterobject::init_types();
    super_::init_types();
    dictview::init_types();
    bootstrap_builtins::init_types();
    frameobject_proxy::init_types();
    use typeobject::type_ready;
    type_ready(typeobject::object_type())?;
    type_ready(typeobject::type_type())?;
    for t in [
        str::str_type(),
        dict::dict_type(),
        tuple::tuple_type(),
        list::list_type(),
        int::int_type(),
        bool_::bool_type(),
        float::float_type(),
        complex::complex_type(),
        bytes::bytes_type(),
        bytearray::bytearray_type(),
        set::set_type(),
        set::frozenset_type(),
        slice::slice_type(),
        &raw mut none::_PyNone_Type,
        &raw mut none::_PyNotImplemented_Type,
        &raw mut none::PyEllipsis_Type,
        descr::method_descr_type(),
        descr::classmethod_descr_type(),
        descr::member_descr_type(),
        descr::getset_descr_type(),
        descr::wrapper_descr_type(),
        descr::method_wrapper_type(),
        cfunction::cfunction_type(),
        cfunction::cmethod_type(),
        funcobject::function_type(),
        funcobject::method_type(),
        funcobject::staticmethod_type(),
        funcobject::classmethod_type(),
        funcobject::cell_type(),
        code::code_type(),
        module::module_type(),
        frameobject::frame_type(),
        traceback::traceback_type(),
        genobject::gen_type(),
        genobject::coro_type(),
        genobject::asyncgen_type(),
        iterobject::seqiter_type(),
        super_::super_type(),
    ] {
        type_ready(t)?;
    }
    exceptions::ready_all()?;
    dictview::ready()?;
pub mod complex_pow;
pub mod bootstrap_builtins;
pub mod structseq;
pub mod lang_util;
pub mod genericalias;
    bootstrap_builtins::ready()?;
    type_ready(frameobject_proxy::proxy_type())?;
    str_methods::ready_types()?;
    Ok(())
}
    lang_init()?;

/// Types of the "lang" workstream (GenericAlias, UnionType, typing objects, async gens).
fn lang_init() -> crate::errors::PyResult<()> {
    genericalias::init_types();
    union::init_types();
    typevar::init_types();
    genericalias::ready()?;
    union::ready()?;
    typevar::ready()?;
    asyncgen::init_types()?;
    genericalias::register_builtin_class_getitems();
