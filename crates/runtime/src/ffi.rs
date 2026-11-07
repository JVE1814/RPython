//! Exact `#[repr(C)]` mirrors of the CPython 3.15 free-threaded ABI structures
//! (from spec/Include). Field order/size must never change.
#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use std::sync::atomic::{AtomicIsize, AtomicPtr, AtomicU16, AtomicU32, AtomicU8, AtomicUsize};

pub type Py_ssize_t = isize;
pub type Py_hash_t = isize;

#[repr(C)]
pub struct PyObject {
    pub ob_tid: AtomicUsize,
    pub ob_flags: AtomicU16,
    pub ob_mutex: AtomicU8,
    pub ob_gc_bits: AtomicU8,
    pub ob_ref_local: AtomicU32,
    pub ob_ref_shared: AtomicIsize,
    pub ob_type: AtomicPtr<PyTypeObject>,
}

#[repr(C)]
pub struct PyVarObject {
    pub ob_base: PyObject,
    pub ob_size: Py_ssize_t,
}

pub type unaryfunc = Option<unsafe extern "C" fn(*mut PyObject) -> *mut PyObject>;
pub type binaryfunc = Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject>;
pub type ternaryfunc =
    Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject>;
pub type inquiry = Option<unsafe extern "C" fn(*mut PyObject) -> c_int>;
pub type lenfunc = Option<unsafe extern "C" fn(*mut PyObject) -> Py_ssize_t>;
pub type ssizeargfunc = Option<unsafe extern "C" fn(*mut PyObject, Py_ssize_t) -> *mut PyObject>;
pub type ssizeobjargproc =
    Option<unsafe extern "C" fn(*mut PyObject, Py_ssize_t, *mut PyObject) -> c_int>;
pub type objobjargproc =
    Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int>;
pub type objobjproc = Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int>;
pub type visitproc = Option<unsafe extern "C" fn(*mut PyObject, *mut c_void) -> c_int>;
pub type traverseproc = Option<unsafe extern "C" fn(*mut PyObject, visitproc, *mut c_void) -> c_int>;
pub type freefunc = Option<unsafe extern "C" fn(*mut c_void)>;
pub type destructor = Option<unsafe extern "C" fn(*mut PyObject)>;
pub type getattrfunc = Option<unsafe extern "C" fn(*mut PyObject, *mut c_char) -> *mut PyObject>;
pub type getattrofunc = Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject>;
pub type setattrfunc =
    Option<unsafe extern "C" fn(*mut PyObject, *mut c_char, *mut PyObject) -> c_int>;
pub type setattrofunc =
    Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int>;
pub type reprfunc = Option<unsafe extern "C" fn(*mut PyObject) -> *mut PyObject>;
pub type hashfunc = Option<unsafe extern "C" fn(*mut PyObject) -> Py_hash_t>;
pub type richcmpfunc =
    Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject, c_int) -> *mut PyObject>;
pub type getiterfunc = Option<unsafe extern "C" fn(*mut PyObject) -> *mut PyObject>;
pub type iternextfunc = Option<unsafe extern "C" fn(*mut PyObject) -> *mut PyObject>;
pub type descrgetfunc =
    Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject>;
pub type descrsetfunc =
    Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int>;
pub type initproc = Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int>;
pub type newfunc =
    Option<unsafe extern "C" fn(*mut PyTypeObject, *mut PyObject, *mut PyObject) -> *mut PyObject>;
pub type allocfunc = Option<unsafe extern "C" fn(*mut PyTypeObject, Py_ssize_t) -> *mut PyObject>;
pub type vectorcallfunc = Option<
    unsafe extern "C" fn(*mut PyObject, *const *mut PyObject, usize, *mut PyObject) -> *mut PyObject,
>;
pub type getbufferproc = Option<unsafe extern "C" fn(*mut PyObject, *mut Py_buffer, c_int) -> c_int>;
pub type releasebufferproc = Option<unsafe extern "C" fn(*mut PyObject, *mut Py_buffer)>;
pub type sendfunc =
    Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut *mut PyObject) -> c_int>;
pub type getter = Option<unsafe extern "C" fn(*mut PyObject, *mut c_void) -> *mut PyObject>;
pub type setter = Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut c_void) -> c_int>;
pub type iteritemfunc = Option<unsafe extern "C" fn(*mut PyObject, Py_ssize_t) -> *mut c_void>;

pub type PyCFunction = Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject>;

#[repr(C)]
pub struct PyNumberMethods {
    pub nb_add: binaryfunc,
    pub nb_subtract: binaryfunc,
    pub nb_multiply: binaryfunc,
    pub nb_remainder: binaryfunc,
    pub nb_divmod: binaryfunc,
    pub nb_power: ternaryfunc,
    pub nb_negative: unaryfunc,
    pub nb_positive: unaryfunc,
    pub nb_absolute: unaryfunc,
    pub nb_bool: inquiry,
    pub nb_invert: unaryfunc,
    pub nb_lshift: binaryfunc,
    pub nb_rshift: binaryfunc,
    pub nb_and: binaryfunc,
    pub nb_xor: binaryfunc,
    pub nb_or: binaryfunc,
    pub nb_int: unaryfunc,
    pub nb_reserved: *mut c_void,
    pub nb_float: unaryfunc,
    pub nb_inplace_add: binaryfunc,
    pub nb_inplace_subtract: binaryfunc,
    pub nb_inplace_multiply: binaryfunc,
    pub nb_inplace_remainder: binaryfunc,
    pub nb_inplace_power: ternaryfunc,
    pub nb_inplace_lshift: binaryfunc,
    pub nb_inplace_rshift: binaryfunc,
    pub nb_inplace_and: binaryfunc,
    pub nb_inplace_xor: binaryfunc,
    pub nb_inplace_or: binaryfunc,
    pub nb_floor_divide: binaryfunc,
    pub nb_true_divide: binaryfunc,
    pub nb_inplace_floor_divide: binaryfunc,
    pub nb_inplace_true_divide: binaryfunc,
    pub nb_index: unaryfunc,
    pub nb_matrix_multiply: binaryfunc,
    pub nb_inplace_matrix_multiply: binaryfunc,
}

#[repr(C)]
pub struct PySequenceMethods {
    pub sq_length: lenfunc,
    pub sq_concat: binaryfunc,
    pub sq_repeat: ssizeargfunc,
    pub sq_item: ssizeargfunc,
    pub was_sq_slice: *mut c_void,
    pub sq_ass_item: ssizeobjargproc,
    pub was_sq_ass_slice: *mut c_void,
    pub sq_contains: objobjproc,
    pub sq_inplace_concat: binaryfunc,
    pub sq_inplace_repeat: ssizeargfunc,
}

#[repr(C)]
pub struct PyMappingMethods {
    pub mp_length: lenfunc,
    pub mp_subscript: binaryfunc,
    pub mp_ass_subscript: objobjargproc,
}

#[repr(C)]
pub struct PyAsyncMethods {
    pub am_await: unaryfunc,
    pub am_aiter: unaryfunc,
    pub am_anext: unaryfunc,
    pub am_send: sendfunc,
}

#[repr(C)]
pub struct PyBufferProcs {
    pub bf_getbuffer: getbufferproc,
    pub bf_releasebuffer: releasebufferproc,
}

#[repr(C)]
pub struct Py_buffer {
    pub buf: *mut c_void,
    pub obj: *mut PyObject,
    pub len: Py_ssize_t,
    pub itemsize: Py_ssize_t,
    pub readonly: c_int,
    pub ndim: c_int,
    pub format: *mut c_char,
    pub shape: *mut Py_ssize_t,
    pub strides: *mut Py_ssize_t,
    pub suboffsets: *mut Py_ssize_t,
    pub internal: *mut c_void,
}

#[repr(C)]
pub struct PyTypeObject {
    pub ob_base: PyVarObject,
    pub tp_name: *const c_char,
    pub tp_basicsize: Py_ssize_t,
    pub tp_itemsize: Py_ssize_t,
    pub tp_dealloc: destructor,
    pub tp_vectorcall_offset: Py_ssize_t,
    pub tp_getattr: getattrfunc,
    pub tp_setattr: setattrfunc,
    pub tp_as_async: *mut PyAsyncMethods,
    pub tp_repr: reprfunc,
    pub tp_as_number: *mut PyNumberMethods,
    pub tp_as_sequence: *mut PySequenceMethods,
    pub tp_as_mapping: *mut PyMappingMethods,
    pub tp_hash: hashfunc,
    pub tp_call: ternaryfunc,
    pub tp_str: reprfunc,
    pub tp_getattro: getattrofunc,
    pub tp_setattro: setattrofunc,
    pub tp_as_buffer: *mut PyBufferProcs,
    pub tp_flags: c_ulong,
    pub tp_doc: *const c_char,
    pub tp_traverse: traverseproc,
    pub tp_clear: inquiry,
    pub tp_richcompare: richcmpfunc,
    pub tp_weaklistoffset: Py_ssize_t,
    pub tp_iter: getiterfunc,
    pub tp_iternext: iternextfunc,
    pub tp_methods: *mut PyMethodDef,
    pub tp_members: *mut PyMemberDef,
    pub tp_getset: *mut PyGetSetDef,
    pub tp_base: *mut PyTypeObject,
    pub tp_dict: *mut PyObject,
    pub tp_descr_get: descrgetfunc,
    pub tp_descr_set: descrsetfunc,
    pub tp_dictoffset: Py_ssize_t,
    pub tp_init: initproc,
    pub tp_alloc: allocfunc,
    pub tp_new: newfunc,
    pub tp_free: freefunc,
    pub tp_is_gc: inquiry,
    pub tp_bases: *mut PyObject,
    pub tp_mro: *mut PyObject,
    pub tp_cache: *mut PyObject,
    pub tp_subclasses: *mut c_void,
    pub tp_weaklist: *mut PyObject,
    pub tp_del: destructor,
    pub tp_version_tag: c_uint,
    pub tp_finalize: destructor,
    pub tp_vectorcall: vectorcallfunc,
    pub tp_watched: u8,
    pub tp_versions_used: u16,
    pub _tp_iteritem: iteritemfunc,
}

#[repr(C)]
pub struct SpecializationCache {
    pub getitem: *mut PyObject,
    pub getitem_version: u32,
    pub init: *mut PyObject,
}

#[repr(C)]
pub struct PyHeapTypeObject {
    pub ht_type: PyTypeObject,
    pub as_async: PyAsyncMethods,
    pub as_number: PyNumberMethods,
    pub as_mapping: PyMappingMethods,
    pub as_sequence: PySequenceMethods,
    pub as_buffer: PyBufferProcs,
    pub ht_name: *mut PyObject,
    pub ht_slots: *mut PyObject,
    pub ht_qualname: *mut PyObject,
    pub ht_cached_keys: *mut c_void,
    pub ht_module: *mut PyObject,
    pub _ht_tpname: *mut c_char,
    pub ht_token: *mut c_void,
    pub _spec_cache: SpecializationCache,
    pub unique_id: Py_ssize_t,
}

#[repr(C)]
pub struct PyMethodDef {
    pub ml_name: *const c_char,
    pub ml_meth: *const c_void,
    pub ml_flags: c_int,
    pub ml_doc: *const c_char,
}
unsafe impl Sync for PyMethodDef {}

#[repr(C)]
pub struct PyMemberDef {
    pub name: *const c_char,
    pub type_code: c_int,
    pub offset: Py_ssize_t,
    pub flags: c_int,
    pub doc: *const c_char,
}
unsafe impl Sync for PyMemberDef {}

#[repr(C)]
pub struct PyGetSetDef {
    pub name: *const c_char,
    pub get: getter,
    pub set: setter,
    pub doc: *const c_char,
    pub closure: *mut c_void,
}
unsafe impl Sync for PyGetSetDef {}

#[repr(C)]
pub struct PyModuleDef_Base {
    pub ob_base: PyObject,
    pub m_init: Option<unsafe extern "C" fn() -> *mut PyObject>,
    pub m_index: Py_ssize_t,
    pub m_copy: *mut PyObject,
}

#[repr(C)]
pub struct PyModuleDef_Slot {
    pub slot: c_int,
    pub value: *mut c_void,
}

#[repr(C)]
pub struct PyModuleDef {
    pub m_base: PyModuleDef_Base,
    pub m_name: *const c_char,
    pub m_doc: *const c_char,
    pub m_size: Py_ssize_t,
    pub m_methods: *mut PyMethodDef,
    pub m_slots: *mut PyModuleDef_Slot,
    pub m_traverse: traverseproc,
    pub m_clear: inquiry,
    pub m_free: freefunc,
}

#[repr(C)]
pub struct PyType_Slot {
    pub slot: c_int,
    pub pfunc: *mut c_void,
}

#[repr(C)]
pub struct PyType_Spec {
    pub name: *const c_char,
    pub basicsize: c_int,
    pub itemsize: c_int,
    pub flags: c_uint,
    pub slots: *mut PyType_Slot,
}

// ---- concrete object layouts ----

#[repr(C)]
pub struct PyLongObject {
    pub ob_base: PyObject,
    pub lv_tag: usize,
    pub ob_digit: [u32; 1],
}

#[repr(C)]
pub struct PyFloatObject {
    pub ob_base: PyObject,
    pub ob_fval: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Py_complex {
    pub real: f64,
    pub imag: f64,
}

#[repr(C)]
pub struct PyComplexObject {
    pub ob_base: PyObject,
    pub cval: Py_complex,
}

#[repr(C)]
pub struct PyTupleObject {
    pub ob_base: PyVarObject,
    pub ob_hash: Py_hash_t,
    pub ob_item: [*mut PyObject; 1],
}

#[repr(C)]
pub struct PyListObject {
    pub ob_base: PyVarObject,
    pub ob_item: *mut *mut PyObject,
    pub allocated: Py_ssize_t,
}

#[repr(C)]
pub struct PyBytesObject {
    pub ob_base: PyVarObject,
    pub ob_shash: Py_hash_t,
    pub ob_sval: [u8; 1],
}

#[repr(C)]
pub struct PyByteArrayObject {
    pub ob_base: PyVarObject,
    pub ob_alloc: Py_ssize_t,
    pub ob_bytes: *mut u8,
    pub ob_start: *mut u8,
    pub ob_exports: Py_ssize_t,
    pub ob_bytes_object: *mut PyObject,
}

#[repr(C)]
pub struct PyASCIIObject {
    pub ob_base: PyObject,
    pub length: Py_ssize_t,
    pub hash: Py_hash_t,
    /// byte 0: interned; bits 8..11 kind, bit 11 compact, bit 12 ascii, bit 13 static.
    pub state: u32,
}

#[repr(C)]
pub struct PyCompactUnicodeObject {
    pub _base: PyASCIIObject,
    pub utf8_length: Py_ssize_t,
    pub utf8: *mut c_char,
}

#[repr(C)]
pub struct PyUnicodeObject {
    pub _base: PyCompactUnicodeObject,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct PyDictObject {
    pub ob_base: PyObject,
    pub ma_used: Py_ssize_t,
    pub _ma_watcher_tag: u64,
    pub ma_keys: *mut c_void,
    pub ma_values: *mut c_void,
}

#[repr(C)]
pub struct PyCFunctionObject {
    pub ob_base: PyObject,
    pub m_ml: *mut PyMethodDef,
    pub m_self: *mut PyObject,
    pub m_module: *mut PyObject,
    pub m_weakreflist: *mut PyObject,
    pub vectorcall: vectorcallfunc,
}

#[repr(C)]
pub struct PyCMethodObject {
    pub func: PyCFunctionObject,
    pub mm_class: *mut PyTypeObject,
}

#[repr(C)]
pub struct PyDescrObject {
    pub ob_base: PyObject,
    pub d_type: *mut PyTypeObject,
    pub d_name: *mut PyObject,
    pub d_qualname: *mut PyObject,
}

#[repr(C)]
pub struct PyMethodDescrObject {
    pub d_common: PyDescrObject,
    pub d_method: *mut PyMethodDef,
    pub vectorcall: vectorcallfunc,
}

#[repr(C)]
pub struct PyMemberDescrObject {
    pub d_common: PyDescrObject,
    pub d_member: *mut PyMemberDef,
}

#[repr(C)]
pub struct PyGetSetDescrObject {
    pub d_common: PyDescrObject,
    pub d_getset: *mut PyGetSetDef,
}

#[repr(C)]
pub struct wrapperbase {
    pub name: *const c_char,
    pub offset: c_int,
    pub function: *mut c_void,
    pub wrapper: *mut c_void,
    pub doc: *const c_char,
    pub flags: c_int,
    pub name_strobj: *mut PyObject,
}

#[repr(C)]
pub struct PyWrapperDescrObject {
    pub d_common: PyDescrObject,
    pub d_base: *mut wrapperbase,
    pub d_wrapped: *mut c_void,
}

#[repr(C)]
pub struct PySliceObject {
    pub ob_base: PyObject,
    pub start: *mut PyObject,
    pub stop: *mut PyObject,
    pub step: *mut PyObject,
}

#[repr(C)]
pub struct PyMethodObject {
    pub ob_base: PyObject,
    pub im_func: *mut PyObject,
    pub im_self: *mut PyObject,
    pub im_weakreflist: *mut PyObject,
    pub vectorcall: vectorcallfunc,
}

// ---- flags ----
pub const Py_TPFLAGS_HAVE_FINALIZE: c_ulong = 1 << 0;
pub const Py_TPFLAGS_INLINE_VALUES: c_ulong = 1 << 2;
pub const Py_TPFLAGS_MANAGED_WEAKREF: c_ulong = 1 << 3;
pub const Py_TPFLAGS_MANAGED_DICT: c_ulong = 1 << 4;
pub const Py_TPFLAGS_PREHEADER: c_ulong = Py_TPFLAGS_MANAGED_WEAKREF | Py_TPFLAGS_MANAGED_DICT;
pub const Py_TPFLAGS_SEQUENCE: c_ulong = 1 << 5;
pub const Py_TPFLAGS_MAPPING: c_ulong = 1 << 6;
pub const Py_TPFLAGS_DISALLOW_INSTANTIATION: c_ulong = 1 << 7;
pub const Py_TPFLAGS_IMMUTABLETYPE: c_ulong = 1 << 8;
pub const Py_TPFLAGS_HEAPTYPE: c_ulong = 1 << 9;
pub const Py_TPFLAGS_BASETYPE: c_ulong = 1 << 10;
pub const Py_TPFLAGS_HAVE_VECTORCALL: c_ulong = 1 << 11;
pub const Py_TPFLAGS_READY: c_ulong = 1 << 12;
pub const Py_TPFLAGS_READYING: c_ulong = 1 << 13;
pub const Py_TPFLAGS_HAVE_GC: c_ulong = 1 << 14;
pub const Py_TPFLAGS_METHOD_DESCRIPTOR: c_ulong = 1 << 17;
pub const Py_TPFLAGS_HAVE_VERSION_TAG: c_ulong = 1 << 18;
pub const Py_TPFLAGS_VALID_VERSION_TAG: c_ulong = 1 << 19;
pub const Py_TPFLAGS_IS_ABSTRACT: c_ulong = 1 << 20;
pub const Py_TPFLAGS_ITEMS_AT_END: c_ulong = 1 << 23;
pub const Py_TPFLAGS_LONG_SUBCLASS: c_ulong = 1 << 24;
pub const Py_TPFLAGS_LIST_SUBCLASS: c_ulong = 1 << 25;
pub const Py_TPFLAGS_TUPLE_SUBCLASS: c_ulong = 1 << 26;
pub const Py_TPFLAGS_BYTES_SUBCLASS: c_ulong = 1 << 27;
pub const Py_TPFLAGS_UNICODE_SUBCLASS: c_ulong = 1 << 28;
pub const Py_TPFLAGS_DICT_SUBCLASS: c_ulong = 1 << 29;
pub const Py_TPFLAGS_BASE_EXC_SUBCLASS: c_ulong = 1 << 30;
pub const Py_TPFLAGS_TYPE_SUBCLASS: c_ulong = 1 << 31;
pub const Py_TPFLAGS_DEFAULT: c_ulong = Py_TPFLAGS_HAVE_VERSION_TAG;

pub const METH_VARARGS: c_int = 0x0001;
pub const METH_KEYWORDS: c_int = 0x0002;
pub const METH_NOARGS: c_int = 0x0004;
pub const METH_O: c_int = 0x0008;
pub const METH_CLASS: c_int = 0x0010;
pub const METH_STATIC: c_int = 0x0020;
pub const METH_COEXIST: c_int = 0x0040;
pub const METH_FASTCALL: c_int = 0x0080;
pub const METH_METHOD: c_int = 0x0200;
/// Private (runtime-only) flag: the call holds `objlock::lock(self)` (see objlock.rs).
pub const METH_LOCKSELF: c_int = 0x0100_0000;
/// Private flag: as METH_LOCKSELF with `objlock::lock_held` (never suspended).
pub const METH_LOCKSELF_HELD: c_int = 0x0200_0000;

pub const Py_T_SHORT: c_int = 0;
pub const Py_T_INT: c_int = 1;
pub const Py_T_LONG: c_int = 2;
pub const Py_T_FLOAT: c_int = 3;
pub const Py_T_DOUBLE: c_int = 4;
pub const Py_T_STRING: c_int = 5;
pub const Py_T_OBJECT: c_int = 6;
pub const Py_T_CHAR: c_int = 7;
pub const Py_T_BYTE: c_int = 8;
pub const Py_T_UBYTE: c_int = 9;
pub const Py_T_USHORT: c_int = 10;
pub const Py_T_UINT: c_int = 11;
pub const Py_T_ULONG: c_int = 12;
pub const Py_T_STRING_INPLACE: c_int = 13;
pub const Py_T_BOOL: c_int = 14;
pub const Py_T_OBJECT_EX: c_int = 16;
pub const Py_T_LONGLONG: c_int = 17;
pub const Py_T_ULONGLONG: c_int = 18;
pub const Py_T_PYSSIZET: c_int = 19;
pub const Py_T_NONE: c_int = 20;
pub const Py_READONLY: c_int = 1;
pub const Py_AUDIT_READ: c_int = 2;
pub const Py_RELATIVE_OFFSET: c_int = 8;

pub const Py_LT: c_int = 0;
pub const Py_LE: c_int = 1;
pub const Py_EQ: c_int = 2;
pub const Py_NE: c_int = 3;
pub const Py_GT: c_int = 4;
pub const Py_GE: c_int = 5;

pub const PY_VECTORCALL_ARGUMENTS_OFFSET: usize = 1usize << (usize::BITS - 1);

// refcount constants (free-threaded)
pub const IMMORTAL_REFCNT_LOCAL: u32 = u32::MAX;
pub const REF_SHARED_SHIFT: u32 = 2;
pub const REF_SHARED_FLAG_MASK: isize = 0x3;
pub const REF_MAYBE_WEAKREF: isize = 0x1;
pub const REF_QUEUED: isize = 0x2;
pub const REF_MERGED: isize = 0x3;

impl PyTypeObject {
    pub const ZERO: PyTypeObject = unsafe { std::mem::zeroed() };
}
impl PyNumberMethods {
    pub const ZERO: PyNumberMethods = unsafe { std::mem::zeroed() };
}
impl PySequenceMethods {
    pub const ZERO: PySequenceMethods = unsafe { std::mem::zeroed() };
}
impl PyMappingMethods {
    pub const ZERO: PyMappingMethods = unsafe { std::mem::zeroed() };
}
impl PyAsyncMethods {
    pub const ZERO: PyAsyncMethods = unsafe { std::mem::zeroed() };
}
impl PyBufferProcs {
    pub const ZERO: PyBufferProcs = unsafe { std::mem::zeroed() };
}

const _: () = {
    assert!(std::mem::size_of::<PyObject>() == 32);
    assert!(std::mem::size_of::<PyASCIIObject>() == 56);
    assert!(std::mem::size_of::<PyCompactUnicodeObject>() == 72);
    assert!(std::mem::offset_of!(PyLongObject, ob_digit) == 40);
    assert!(std::mem::offset_of!(PyTupleObject, ob_item) == 48);
};
