//! The adaptive view of public code objects (`_co_code_adaptive`, `dis(adaptive=True)`).
//!
//! CPython rewrites generic instructions into specialised ones as it observes operand
//! types. Our interpreter specialises differently (side-table inline caches, JIT tiers),
//! so the specialised *names* are only a view: tier 0 records, per internal instruction,
//! the specialised CPython opcode its operands would have selected (one byte; the last
//! observation wins, so operands no specialisation covers show the generic name again,
//! like CPython after a failed guard).
//! `adaptive_code` writes the recorded opcodes into a copy of co_code.

use crate::ffi::*;
use crate::object::*;
use crate::types::code::CodeData;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, float, frozendict, funcobject, genobject, int, list, none, set, str as pystr, tuple};
use rpy_compiler::wordcode::{self as wc, opcodes as o};
use std::sync::atomic::Ordering::Relaxed;

/// Marker: observed operands that no specialisation covers.
pub const GENERIC: u8 = 1;

/// Record observation `k` (a specialised opcode, or GENERIC) for internal pc `pc`.
#[inline(always)]
pub fn note(cd: &CodeData, pc: usize, k: u16) {
    if let Some(s) = cd.spec.get(pc) {
        let old = s.load(Relaxed);
        let k = k as u8;
        if old != k {
            s.store(k, Relaxed);
        }
    }
}

/// Has this instruction already been classified (generic or specialised)? Callers skip
/// expensive classifications then.
#[inline(always)]
pub fn seen(cd: &CodeData, pc: usize) -> bool {
    cd.spec.get(pc).is_none_or(|s| s.load(Relaxed) != 0)
}

#[inline(always)]
pub fn is_generic(cd: &CodeData, pc: usize) -> bool {
    cd.spec.get(pc).is_none_or(|s| s.load(Relaxed) == GENERIC)
}

#[inline(always)]
pub fn active(cd: &CodeData) -> bool {
    !cd.spec.is_empty()
}

const G: u16 = GENERIC as u16;

fn compact_int(x: &PyObject) -> bool {
    int::is_int_exact(x) && int::is_compact(x)
}

fn tp_name(x: &PyObject) -> &str {
    unsafe { std::ffi::CStr::from_ptr(x.ty().tp_name).to_str().unwrap_or("") }
}

/// BINARY_OP with NB_* `nb` on (a, b).
pub fn binary(nb: u32, a: &PyObject, b: &PyObject) -> u16 {
    let base = if nb >= 13 && nb < 26 { nb - 13 } else { nb };
    if nb == 26 {
        return subscr(a, b);
    }
    // NB_ADD 0, NB_AND 1, NB_MULTIPLY 5, NB_OR 7, NB_SUBTRACT 10, NB_TRUE_DIVIDE 11, NB_XOR 12
    if compact_int(a) && compact_int(b) {
        return match base {
            0 => o::BINARY_OP_ADD_INT,
            10 => o::BINARY_OP_SUBTRACT_INT,
            5 => o::BINARY_OP_MULTIPLY_INT,
            1 | 7 | 12 => o::BINARY_OP_EXTEND,
            _ => G,
        };
    }
    if float::is_float_exact(a) && float::is_float_exact(b) {
        return match base {
            0 => o::BINARY_OP_ADD_FLOAT,
            10 => o::BINARY_OP_SUBTRACT_FLOAT,
            5 => o::BINARY_OP_MULTIPLY_FLOAT,
            _ => G,
        };
    }
    let mixed = (compact_int(a) && float::is_float_exact(b)) || (float::is_float_exact(a) && compact_int(b));
    if mixed {
        let fv = if float::is_float_exact(a) { float::float_value(a) } else { float::float_value(b) };
        if fv.is_nan() {
            return G;
        }
        return match base {
            0 | 10 | 5 => o::BINARY_OP_EXTEND,
            11 => {
                let zero = if float::is_float_exact(b) { float::float_value(b) == 0.0 } else { int::compact_value(b) == 0 };
                if zero { G } else { o::BINARY_OP_EXTEND }
            }
            _ => G,
        };
    }
    if pystr::is_str_exact(a) && pystr::is_str_exact(b) && base == 0 {
        return if nb == 13 { o::BINARY_OP_INPLACE_ADD_UNICODE } else { o::BINARY_OP_ADD_UNICODE };
    }
    if base == 0 && ((list::is_list_exact(a) && list::is_list_exact(b)) || (tuple::is_tuple_exact(a) && tuple::is_tuple_exact(b))) {
        return o::BINARY_OP_EXTEND;
    }
    G
}

fn subscr(c: &PyObject, k: &PyObject) -> u16 {
    if list::is_list_exact(c) {
        if compact_int(k) {
            return o::BINARY_OP_SUBSCR_LIST_INT;
        }
        if k.is_type(crate::types::slice::slice_type()) {
            return o::BINARY_OP_SUBSCR_LIST_SLICE;
        }
        return G;
    }
    if tuple::is_tuple_exact(c) {
        return if compact_int(k) { o::BINARY_OP_SUBSCR_TUPLE_INT } else { G };
    }
    if pystr::is_str_exact(c) {
        if !compact_int(k) {
            return G;
        }
        return if pystr::str_kind(c) == 1 { o::BINARY_OP_SUBSCR_STR_INT } else { o::BINARY_OP_SUBSCR_USTR_INT };
    }
    if dict::is_dict(c) || frozendict::is_frozendict(c) {
        let base_slot = if dict::is_dict(c) { dict::dict_type() } else { frozendict::frozendict_type() };
        let own = unsafe { (*c.ty().tp_as_mapping).mp_subscript.map(|f| f as usize) };
        let base = unsafe { (*(*base_slot).tp_as_mapping).mp_subscript.map(|f| f as usize) };
        if own == base || lookup(c.type_ptr(), crate::name!("__getitem__")).is_some_and(|g| !funcobject::is_function(&g)) {
            return o::BINARY_OP_SUBSCR_DICT;
        }
    }
    if c.ty().tp_flags & Py_TPFLAGS_HEAPTYPE != 0 {
        if let Some(g) = lookup(c.type_ptr(), crate::name!("__getitem__")) {
            if funcobject::is_function(&g) {
                return o::BINARY_OP_SUBSCR_GETITEM;
            }
        }
    }
    G
}

pub fn compare(cmp: u32, a: &PyObject, b: &PyObject) -> u16 {
    if compact_int(a) && compact_int(b) {
        return o::COMPARE_OP_INT;
    }
    if float::is_float_exact(a) && float::is_float_exact(b) {
        return o::COMPARE_OP_FLOAT;
    }
    if pystr::is_str_exact(a) && pystr::is_str_exact(b) && (cmp == 2 || cmp == 3) {
        return o::COMPARE_OP_STR;
    }
    G
}

pub fn contains(container: &PyObject) -> u16 {
    if dict::is_dict_exact(container) || frozendict::is_frozendict_exact(container) {
        return o::CONTAINS_OP_DICT;
    }
    if set::is_anyset_exact(container) {
        return o::CONTAINS_OP_SET;
    }
    G
}

pub fn to_bool(v: &PyObject) -> u16 {
    if bool_::is_bool(v) {
        return o::TO_BOOL_BOOL;
    }
    if int::is_int_exact(v) {
        return o::TO_BOOL_INT;
    }
    if list::is_list_exact(v) {
        return o::TO_BOOL_LIST;
    }
    if none::is_none(v) {
        return o::TO_BOOL_NONE;
    }
    if pystr::is_str_exact(v) {
        return o::TO_BOOL_STR;
    }
    let tp = v.ty();
    let no_len = |m: *mut PyMappingMethods, s: *mut PySequenceMethods| unsafe {
        (m.is_null() || (*m).mp_length.is_none()) && (s.is_null() || (*s).sq_length.is_none())
    };
    let no_bool = tp.tp_as_number.is_null() || unsafe { (*tp.tp_as_number).nb_bool.is_none() };
    if tp.tp_flags & Py_TPFLAGS_HEAPTYPE != 0 && no_bool && no_len(tp.tp_as_mapping, tp.tp_as_sequence) {
        return o::TO_BOOL_ALWAYS_TRUE;
    }
    G
}

pub fn unpack(seq: &PyObject, n: u32) -> u16 {
    if tuple::is_tuple_exact(seq) && tuple::tuple_len(seq) == n as usize {
        return if n == 2 { o::UNPACK_SEQUENCE_TWO_TUPLE } else { o::UNPACK_SEQUENCE_TUPLE };
    }
    if list::is_list_exact(seq) && list::list_len(seq) == n as usize {
        return o::UNPACK_SEQUENCE_LIST;
    }
    G
}

pub fn store_subscr(c: &PyObject, k: &PyObject) -> u16 {
    if list::is_list_exact(c) && compact_int(k) {
        return o::STORE_SUBSCR_LIST_INT;
    }
    if frozendict::is_frozendict_exact(c) {
        return o::STORE_SUBSCR_DICT;
    }
    if dict::is_dict(c) {
        let own = unsafe { (*c.ty().tp_as_mapping).mp_ass_subscript.map(|f| f as usize) };
        let base = unsafe { (*(*dict::dict_type()).tp_as_mapping).mp_ass_subscript.map(|f| f as usize) };
        if own == base {
            return o::STORE_SUBSCR_DICT;
        }
    }
    G
}

/// GET_ITER on `iterable` (oparg 0).
pub fn get_iter(iterable: &PyObject) -> u16 {
    if list::is_list_exact(iterable) || tuple::is_tuple_exact(iterable) {
        return o::GET_ITER_VIRTUAL;
    }
    if genobject::is_generator_exact(iterable) {
        return o::GET_ITER_SELF;
    }
    G
}

/// FOR_ITER over iterator `it`.
pub fn for_iter(it: &PyObject) -> u16 {
    if genobject::is_generator_exact(it) {
        return o::FOR_ITER_GEN;
    }
    match tp_name(it) {
        "list_iterator" => o::FOR_ITER_LIST,
        "tuple_iterator" => o::FOR_ITER_TUPLE,
        "range_iterator" => o::FOR_ITER_RANGE,
        "str_ascii_iterator" | "str_iterator" => o::FOR_ITER_VIRTUAL,
        _ => G,
    }
}

/// SEND to receiver `r`.
pub fn send(r: &PyObject) -> u16 {
    if genobject::is_generator_exact(r) || genobject::is_coroutine_exact(r) {
        return o::SEND_GEN;
    }
    match tp_name(r) {
        "async_generator_asend" => o::SEND_ASYNC_GEN,
        "list_iterator" | "tuple_iterator" => o::SEND_VIRTUAL,
        _ => G,
    }
}

fn simple_py_function(f: &PyObject, nargs: usize) -> bool {
    let fo = funcobject::func(f);
    let co = crate::types::code::code(unsafe { &*fo.func_code });
    let flags = co.flags();
    flags & (rpy_compiler::bytecode::co::VARARGS | rpy_compiler::bytecode::co::VARKEYWORDS) == 0
        && co.obj.co_kwonlyargcount == 0
        && co.obj.co_argcount as usize == nargs
}

fn has_python_vectorcall(f: &PyObject) -> bool {
    funcobject::func(f).vectorcall.map(|v| v as usize) == Some(crate::interp::function_vectorcall as usize)
}

fn method_func(m: &PyObject) -> PyRef {
    unsafe { PyRef::from_borrowed((*(m.as_ptr() as *const PyMethodObject)).im_func) }
}

/// CALL: callable, self_or_null, number of positional arguments (excluding self).
pub fn call(callable: &PyObject, self_or_null: Option<&PyObject>, nargs: usize) -> u16 {
    let total = nargs + self_or_null.is_some() as usize;
    if funcobject::is_function(callable) {
        if !has_python_vectorcall(callable) {
            return o::CALL_NON_PY_GENERAL;
        }
        return if simple_py_function(callable, total) { o::CALL_PY_EXACT_ARGS } else { o::CALL_PY_GENERAL };
    }
    if funcobject::is_method(callable) && self_or_null.is_none() {
        let f = method_func(callable);
        if funcobject::is_function(&f) {
            return if simple_py_function(&f, nargs + 1) { o::CALL_BOUND_METHOD_EXACT_ARGS } else { o::CALL_BOUND_METHOD_GENERAL };
        }
        return o::CALL_NON_PY_GENERAL;
    }
    if is_type(callable) {
        let t = as_type(callable);
        if total == 1 && self_or_null.is_none() {
            if t == type_type() {
                return o::CALL_TYPE_1;
            }
            if t == pystr::str_type() {
                return o::CALL_STR_1;
            }
            if t == tuple::tuple_type() {
                return o::CALL_TUPLE_1;
            }
        }
        let tp = unsafe { &*t };
        if tp.tp_flags & Py_TPFLAGS_HEAPTYPE != 0 {
            let init = lookup(t, crate::name!("__init__"));
            let new_is_object = lookup(t, crate::name!("__new__")).is_some_and(|n| {
                lookup(crate::types::typeobject::object_type(), crate::name!("__new__")).is_some_and(|b| b.is(&n))
            });
            if callable.type_ptr() == type_type() && new_is_object && init.is_some_and(|i| funcobject::is_function(&i)) {
                return o::CALL_ALLOC_AND_ENTER_INIT;
            }
            return o::CALL_NON_PY_GENERAL;
        }
        return o::CALL_BUILTIN_CLASS;
    }
    if crate::types::cfunction::is_cfunction(callable) && self_or_null.is_none() {
        let cf = unsafe { &*(callable.as_ptr() as *const PyCFunctionObject) };
        let name = unsafe { std::ffi::CStr::from_ptr((*cf.m_ml).ml_name).to_str().unwrap_or("") };
        let module_level = cf.m_self.is_null() || crate::types::module::is_module(unsafe { &*cf.m_self });
        if module_level && name == "len" && nargs == 1 {
            return o::CALL_LEN;
        }
        if module_level && name == "isinstance" && nargs == 2 {
            return o::CALL_ISINSTANCE;
        }
        let fl = unsafe { (*cf.m_ml).ml_flags } & !(METH_CLASS | METH_STATIC | METH_COEXIST);
        return match fl {
            METH_O if nargs == 1 => o::CALL_BUILTIN_O,
            METH_FASTCALL => o::CALL_BUILTIN_FAST,
            x if x == METH_FASTCALL | METH_KEYWORDS => o::CALL_BUILTIN_FAST_WITH_KEYWORDS,
            _ => o::CALL_NON_PY_GENERAL,
        };
    }
    if callable.is_type(crate::types::descr::method_descr_type()) && self_or_null.is_some() {
        let d = unsafe { &*(callable.as_ptr() as *const PyMethodDescrObject) };
        let ml = unsafe { &*d.d_method };
        let name = unsafe { std::ffi::CStr::from_ptr(ml.ml_name).to_str().unwrap_or("") };
        if name == "append" && d.d_common.d_type == list::list_type() && nargs == 1 && self_or_null.is_some_and(list::is_list_exact) {
            return o::CALL_LIST_APPEND;
        }
        let fl = ml.ml_flags & !(METH_CLASS | METH_STATIC | METH_COEXIST);
        return match fl {
            METH_O if nargs == 1 => o::CALL_METHOD_DESCRIPTOR_O,
            METH_NOARGS if nargs == 0 => o::CALL_METHOD_DESCRIPTOR_NOARGS,
            METH_FASTCALL => o::CALL_METHOD_DESCRIPTOR_FAST,
            x if x == METH_FASTCALL | METH_KEYWORDS => o::CALL_METHOD_DESCRIPTOR_FAST_WITH_KEYWORDS,
            _ => o::CALL_NON_PY_GENERAL,
        };
    }
    o::CALL_NON_PY_GENERAL
}

pub fn call_kw(callable: &PyObject, self_or_null: Option<&PyObject>) -> u16 {
    if funcobject::is_function(callable) && has_python_vectorcall(callable) {
        return o::CALL_KW_PY;
    }
    if funcobject::is_method(callable) && self_or_null.is_none() && funcobject::is_function(&method_func(callable)) {
        return o::CALL_KW_BOUND_METHOD;
    }
    o::CALL_KW_NON_PY
}

pub fn call_ex(callable: &PyObject) -> u16 {
    if funcobject::is_function(callable) && has_python_vectorcall(callable) {
        return o::CALL_EX_PY;
    }
    o::CALL_EX_NON_PY_GENERAL
}

/// LOAD_GLOBAL: found in globals (module) or in builtins.
pub fn load_global(globals: &PyObject, builtins: &PyObject, name: &PyObject) -> u16 {
    if !dict::is_dict_exact(globals) || !dict::is_dict_exact(builtins) {
        return G;
    }
    if dict::get_item_str(globals, name).is_some() {
        return o::LOAD_GLOBAL_MODULE;
    }
    if dict::get_item_str(builtins, name).is_some() {
        return o::LOAD_GLOBAL_BUILTIN;
    }
    G
}

fn instance_has_attr(owner: &PyObject, name: &PyObject) -> Option<bool> {
    // Some(true): stored inline (values); Some(false): in a materialised dict
    match crate::shape::get(owner, name) {
        crate::shape::Get::Found(_) => Some(true),
        crate::shape::Get::Missing => None,
        crate::shape::Get::NotValues => {
            let d = instance_dict_existing(owner)?;
            dict::get_item_str(&d, name).map(|_| false)
        }
    }
}

/// LOAD_ATTR (method form when `method`).
pub fn load_attr(owner: &PyObject, name: &PyObject, method: bool) -> u16 {
    if crate::types::module::is_module(owner) {
        if owner.is_type(crate::types::module::module_type()) {
            let d = crate::types::module::module_dict(owner);
            if dict::get_item_str(&d, name).is_some() {
                return o::LOAD_ATTR_MODULE;
            }
        }
        return G;
    }
    if is_type(owner) {
        if method {
            return G;
        }
        let meta = owner.type_ptr();
        if lookup(meta, name).is_some() {
            return G;
        }
        if lookup(as_type(owner), name).is_some() {
            return if meta == type_type() { o::LOAD_ATTR_CLASS } else { o::LOAD_ATTR_CLASS_WITH_METACLASS_CHECK };
        }
        return G;
    }
    let tp = owner.ty();
    let generic = tp.tp_getattro.map(|f| f as usize) == Some(object_generic_getattro as usize);
    if pystr::str_eq(name, crate::name!("__class__")) && generic {
        return o::LOAD_ATTR_SLOT;
    }
    if !generic {
        if tp.tp_flags & Py_TPFLAGS_HEAPTYPE != 0 && lookup(owner.type_ptr(), crate::name!("__getattribute__")).is_some_and(|g| funcobject::is_function(&g)) {
            return o::LOAD_ATTR_GETATTRIBUTE_OVERRIDDEN;
        }
        return G;
    }
    let descr = lookup(owner.type_ptr(), name);
    let has_dict = tp.tp_dictoffset != 0 || tp.tp_flags & Py_TPFLAGS_MANAGED_DICT != 0;
    match &descr {
        Some(d) if d.ty().tp_descr_set.is_some() => {
            if d.is_type(crate::types::descr::member_descr_type()) {
                return if method { G } else { o::LOAD_ATTR_SLOT };
            }
            if d.is_type(crate::types::property::property_type()) && !method {
                return o::LOAD_ATTR_PROPERTY;
            }
            return G;
        }
        Some(d) if method && d.ty().tp_flags & Py_TPFLAGS_METHOD_DESCRIPTOR != 0 => {
            if !has_dict {
                return o::LOAD_ATTR_METHOD_NO_DICT;
            }
            if instance_has_attr(owner, name).is_some() {
                return G;
            }
            if crate::shape::has_inline_values(owner) {
                return o::LOAD_ATTR_METHOD_WITH_VALUES;
            }
            if instance_dict_existing(owner).is_none() {
                return o::LOAD_ATTR_METHOD_LAZY_DICT;
            }
            return G;
        }
        _ => {}
    }
    match instance_has_attr(owner, name) {
        Some(true) => o::LOAD_ATTR_INSTANCE_VALUE,
        Some(false) => o::LOAD_ATTR_WITH_HINT,
        None => match descr {
            Some(d) if d.ty().tp_descr_get.is_none() && !method => {
                if !has_dict {
                    o::LOAD_ATTR_NONDESCRIPTOR_NO_DICT
                } else if crate::shape::has_inline_values(owner) {
                    o::LOAD_ATTR_NONDESCRIPTOR_WITH_VALUES
                } else {
                    G
                }
            }
            _ => G,
        },
    }
}

pub fn store_attr(owner: &PyObject, name: &PyObject) -> u16 {
    let tp = owner.ty();
    if tp.tp_setattro.map(|f| f as usize) != Some(object_generic_setattro as usize) || is_type(owner) {
        return G;
    }
    if let Some(d) = lookup(owner.type_ptr(), name) {
        if d.ty().tp_descr_set.is_some() {
            return if d.is_type(crate::types::descr::member_descr_type()) { o::STORE_ATTR_SLOT } else { G };
        }
    }
    if crate::shape::has_inline_values(owner) {
        return o::STORE_ATTR_INSTANCE_VALUE;
    }
    if instance_dict_existing(owner).is_some() {
        return o::STORE_ATTR_WITH_HINT;
    }
    G
}

pub fn load_super_attr(global_super: &PyObject, method: bool) -> u16 {
    if !global_super.is(type_obj(crate::types::super_::super_type())) {
        return G;
    }
    if method { o::LOAD_SUPER_ATTR_METHOD } else { o::LOAD_SUPER_ATTR_ATTR }
}

/// `_co_code_adaptive`: co_code with every observed instruction replaced by its
/// specialised form (instructions of the same family only).
pub fn adaptive_code(co: &PyObject) -> Vec<u8> {
    let m = crate::types::code::code_meta(co);
    let mut out = crate::types::bytes::bytes_data(&m.co_code).to_vec();
    if !m.public || !crate::types::code::is_translated(co) {
        return out;
    }
    let cd = crate::types::code::code_data(co);
    if cd.spec.is_empty() {
        return out;
    }
    for ins in wc::instructions(&out.clone()) {
        let mut ipc = cd.internal_pc(ins.offset as usize) as usize;
        // skip internal EXTENDED_ARG prefixes
        while let Some(&w) = cd.code.get(ipc) {
            if rpy_compiler::bytecode::decode(w).0 == rpy_compiler::bytecode::Op::ExtendedArg {
                ipc += 1;
            } else {
                break;
            }
        }
        if cd.pub_of_internal.get(ipc) != Some(&ins.offset) {
            continue; // the instruction has no internal word of its own
        }
        let v = cd.spec[ipc].load(Relaxed) as u16;
        if v >= wc::opcodes::MIN_SPECIALIZED_OPCODE && wc::deopt(v) == wc::deopt(ins.op as u16) {
            out[2 * ins.offset as usize] = v as u8;
        }
    }
    out
}
