//! `_ctypes`: C data types and foreign function calls (libffi binding).
//!
//! Object model:
//! * Every ctypes type (Structure subclass, c_int, `c_int * 3`, `POINTER(c_int)`,
//!   `CFUNCTYPE(...)`) is a heap type whose metaclass derives from `CType_Type`.
//!   The metaclass instances are one pointer larger than a heap type; that slot holds
//!   the type's `StgInfo` (size, alignment, libffi type, item/target type, ...).
//!   `StgInfo` is created by the metaclass `__init__`.
//! * Every ctypes instance is a `CDataObject`: a pointer to its memory (`b_ptr`, inline
//!   storage for small objects), the object owning that memory (`b_base`) and the
//!   keep-alive container `b_objects`.

pub mod array;
pub mod cdata;
pub mod cfield;
pub mod funcptr;
pub mod libffi;
pub mod module;
pub mod pointer;
pub mod simple;
pub mod structs;

use crate::capi::{self, Ob};
use libffi::FfiType;
use rpy_runtime::errors::*;
use rpy_runtime::ffi::*;
use rpy_runtime::object::*;
use std::ffi::{CString, c_char, c_int};
use std::sync::atomic::Ordering::Relaxed;

pub use rpy_runtime::{err, raise};

// ------------------------------------------------------------------ flags

pub const FUNCFLAG_STDCALL: i32 = 0x0;
pub const FUNCFLAG_CDECL: i32 = 0x1;
pub const FUNCFLAG_HRESULT: i32 = 0x2;
pub const FUNCFLAG_PYTHONAPI: i32 = 0x4;
pub const FUNCFLAG_USE_ERRNO: i32 = 0x8;
pub const FUNCFLAG_USE_LASTERROR: i32 = 0x10;

pub const TYPEFLAG_ISPOINTER: u32 = 0x100;
pub const TYPEFLAG_HASPOINTER: u32 = 0x200;
pub const TYPEFLAG_HASUNION: u32 = 0x400;
pub const TYPEFLAG_HASBITFIELD: u32 = 0x800;
pub const DICTFLAG_FINAL: u32 = 0x1000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Simple,
    Struct,
    Union,
    Array,
    Pointer,
    FuncPtr,
}

/// Storage information of a ctypes type.
pub struct StgInfo {
    pub initialized: bool,
    pub kind: Kind,
    pub size: isize,
    pub align: isize,
    /// Structures/unions: number of fields (incl. base fields); arrays: item count.
    pub length: isize,
    /// libffi description (boxed: its address must stay stable).
    pub ffi: Box<FfiType>,
    pub ffi_elements: Vec<*mut FfiType>,
    pub ffi_children: Vec<Box<FfiType>>,
    /// Arrays/pointers: the item/target type; simple types: the `_type_` string.
    pub proto: Option<PyRef>,
    /// Simple types: the type code; 0 otherwise.
    pub code: u8,
    /// Simple types: stored with non-native byte order.
    pub swapped: bool,
    pub argtypes: Option<PyRef>,
    pub converters: Option<PyRef>,
    pub restype: Option<PyRef>,
    pub checker: Option<PyRef>,
    pub errcheck: Option<PyRef>,
    pub flags: i32,
    pub dict_flags: u32,
    pub format: Option<String>,
    pub shape: Vec<isize>,
    pub pointer_type: Option<PyRef>,
}

impl StgInfo {
    pub fn new(kind: Kind) -> StgInfo {
        StgInfo {
            initialized: true,
            kind,
            size: 0,
            align: 0,
            length: 0,
            ffi: Box::new(FfiType::zero()),
            ffi_elements: Vec::new(),
            ffi_children: Vec::new(),
            proto: None,
            code: 0,
            swapped: false,
            argtypes: None,
            converters: None,
            restype: None,
            checker: None,
            errcheck: None,
            flags: 0,
            dict_flags: 0,
            format: None,
            shape: Vec::new(),
            pointer_type: None,
        }
    }

    pub fn ffi_ptr(&self) -> *mut FfiType {
        &*self.ffi as *const FfiType as *mut FfiType
    }

    /// Copy used when a subclass inherits a finished layout from its base.
    pub fn clone_for_subclass(&self) -> StgInfo {
        let mut n = StgInfo::new(self.kind);
        n.size = self.size;
        n.align = self.align;
        n.length = self.length;
        n.code = self.code;
        n.swapped = self.swapped;
        n.proto = self.proto.clone();
        n.argtypes = self.argtypes.clone();
        n.converters = self.converters.clone();
        n.restype = self.restype.clone();
        n.checker = self.checker.clone();
        n.errcheck = self.errcheck.clone();
        n.flags = self.flags;
        n.dict_flags = self.dict_flags & !DICTFLAG_FINAL;
        n.format = self.format.clone();
        n.shape = self.shape.clone();
        n.set_ffi_from(self);
        n
    }

    /// Make this StgInfo's libffi type an independent copy of `other`'s.
    pub fn set_ffi_from(&mut self, other: &StgInfo) {
        *self.ffi = *other.ffi;
        if !other.ffi_elements.is_empty() {
            self.ffi_elements = other.ffi_elements.clone();
            self.ffi.elements = self.ffi_elements.as_mut_ptr();
        }
        // children of `other` stay owned by `other`; keep them alive by copying
        self.ffi_children = other.ffi_children.iter().map(|b| Box::new(**b)).collect();
        // re-point elements that referenced other's children
        for (i, c) in other.ffi_children.iter().enumerate() {
            let old = &**c as *const FfiType as *mut FfiType;
            let new = &*self.ffi_children[i] as *const FfiType as *mut FfiType;
            for e in self.ffi_elements.iter_mut() {
                if *e == old {
                    *e = new;
                }
            }
        }
    }

    fn visit(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        for o in [&self.proto, &self.argtypes, &self.converters, &self.restype, &self.checker, &self.errcheck, &self.pointer_type] {
            if let Some(r) = o {
                let x = v(r.as_ptr());
                if x != 0 {
                    return x;
                }
            }
        }
        0
    }

    fn clear(&mut self) {
        self.proto = None;
        self.argtypes = None;
        self.converters = None;
        self.restype = None;
        self.checker = None;
        self.errcheck = None;
        self.pointer_type = None;
    }
}

// ------------------------------------------------------------------ helpers


#[inline]
pub fn ob_type(o: *mut PyObject) -> *mut PyTypeObject {
    unsafe { (*o).ob_type.load(Relaxed) }
}

#[inline]
pub fn r<'a>(p: *mut PyObject) -> &'a PyObject {
    unsafe { &*p }
}

pub fn type_ob(t: *mut PyTypeObject) -> &'static PyObject {
    rpy_runtime::types::typeobject::type_obj(t)
}

pub fn is_type(o: &PyObject) -> bool {
    unsafe { (*o.type_ptr()).tp_flags & Py_TPFLAGS_TYPE_SUBCLASS != 0 }
}

pub fn subtype(a: *mut PyTypeObject, b: *mut PyTypeObject) -> bool {
    rpy_runtime::types::typeobject::is_subtype(a, b)
}

pub fn tname(o: &PyObject) -> String {
    rpy_runtime::types::typeobject::tname(o)
}

/// `type.__name__` of a type object.
pub fn type_name(t: *mut PyTypeObject) -> String {
    unsafe { rpy_runtime::types::typeobject::type_name(&*t) }
}

pub fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "\\0")).unwrap()
}

pub fn from_c(p: *mut PyObject) -> PyResult {
    unsafe { rpy_runtime::errors::from_c(p) }
}

pub fn none() -> PyRef {
    rpy_runtime::types::none::none_ref()
}

pub fn is_none(o: &PyObject) -> bool {
    o.is(rpy_runtime::types::none::none())
}

pub fn new_str(s: &str) -> PyRef {
    rpy_runtime::types::str::new_str(s)
}

pub fn new_int(v: i64) -> PyRef {
    rpy_runtime::types::int::new_int(v)
}

pub fn new_uint(v: u64) -> PyRef {
    rpy_runtime::types::int::new_int_u128(v as u128)
}

pub fn new_ptr_int(p: usize) -> PyRef {
    new_uint(p as u64)
}

pub fn set_err(t: Ob, msg: &str) -> PyErr {
    rpy_runtime::errors::new_err(t as *mut PyTypeObject, msg)
}

pub fn getattr_opt(o: &PyObject, name: &str) -> PyResult<Option<PyRef>> {
    let n = rpy_runtime::types::str::intern_str(name);
    rpy_runtime::abstract_::getattr_opt(o, &n)
}

pub fn getattr(o: &PyObject, name: &str) -> PyResult {
    rpy_runtime::abstract_::getattr_str(o, name)
}

pub fn setattr(o: &PyObject, name: &str, v: &PyObject) -> PyResult<()> {
    let n = rpy_runtime::types::str::intern_str(name);
    rpy_runtime::abstract_::setattr(o, &n, v)
}

pub fn call(f: &PyObject, args: &[&PyObject]) -> PyResult {
    rpy_runtime::abstract_::call(f, args)
}

pub fn repr(o: &PyObject) -> PyResult<String> {
    rpy_runtime::abstract_::repr_string(o)
}

pub fn is_int(o: &PyObject) -> bool {
    rpy_runtime::types::int::is_int(o)
}

pub fn is_bytes(o: &PyObject) -> bool {
    rpy_runtime::types::bytes::is_bytes(o)
}

pub fn is_str(o: &PyObject) -> bool {
    rpy_runtime::types::str::is_str(o)
}

pub fn is_tuple(o: &PyObject) -> bool {
    rpy_runtime::types::tuple::is_tuple(o)
}

pub fn is_dict_exact(o: &PyObject) -> bool {
    unsafe { o.type_ptr() == &raw mut rpy_runtime::types::dict::PyDict_Type }
}

pub fn str_of(o: &PyObject) -> String {
    rpy_runtime::types::str::to_string_lossy(o)
}

pub fn isinstance(o: &PyObject, cls: &PyObject) -> PyResult<bool> {
    rpy_runtime::abstract_::isinstance(o, cls)
}

/// Python int -> u64 with C wrap-around semantics (accepts __index__).
pub fn int_mask(o: &PyObject) -> PyResult<u64> {
    let v = unsafe { capi::PyLong_AsUnsignedLongLongMask(o.as_ptr()) };
    if v == u64::MAX && err_occurred() {
        return Err(PyErr);
    }
    Ok(v)
}

pub fn ssize(o: &PyObject) -> PyResult<isize> {
    rpy_runtime::abstract_::index_as_ssize(o, Some(rpy_runtime::types::exceptions::OverflowError()))
}

// ------------------------------------------------------------------ StgInfo access

/// The metaclass base of all ctypes types.
pub static mut CTYPE_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn ctype_type() -> *mut PyTypeObject {
    &raw mut CTYPE_TYPE
}

const STG_OFFSET: usize = std::mem::size_of::<PyHeapTypeObject>();

/// Pointer to the StgInfo slot of `tp`, if `tp` is a heap type with a ctypes metaclass.
pub fn stg_slot(tp: *mut PyTypeObject) -> Option<*mut *mut StgInfo> {
    unsafe {
        if tp.is_null() || (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE == 0 {
            return None;
        }
        let meta = ob_type(tp as *mut PyObject);
        if !subtype(meta, ctype_type()) {
            return None;
        }
        Some((tp as *mut u8).add(STG_OFFSET) as *mut *mut StgInfo)
    }
}

/// The initialized StgInfo of a type.
pub fn stginfo(tp: *mut PyTypeObject) -> Option<&'static mut StgInfo> {
    let slot = stg_slot(tp)?;
    unsafe {
        let p = *slot;
        if p.is_null() || !(*p).initialized { None } else { Some(&mut *p) }
    }
}

/// StgInfo of a type object given as a PyObject.
pub fn stginfo_of_type(o: &PyObject) -> Option<&'static mut StgInfo> {
    if !is_type(o) {
        return None;
    }
    stginfo(o.as_ptr() as *mut PyTypeObject)
}

/// StgInfo of an instance.
pub fn stginfo_of_obj(o: &PyObject) -> Option<&'static mut StgInfo> {
    stginfo(o.type_ptr())
}

/// StgInfo of a type or instance.
pub fn stginfo_any(o: &PyObject) -> Option<&'static mut StgInfo> {
    if is_type(o) { stginfo_of_type(o) } else { stginfo_of_obj(o) }
}

/// Install a fresh StgInfo on `tp` (SystemError if one exists).
pub fn init_stginfo(tp: *mut PyTypeObject, info: StgInfo) -> PyResult<&'static mut StgInfo> {
    let slot = match stg_slot(tp) {
        Some(s) => s,
        None => raise!(TypeError, "ctypes type expected"),
    };
    unsafe {
        if !(*slot).is_null() {
            raise!(SystemError, "StgInfo of '{}' is already initialized.", type_name(tp));
        }
        *slot = Box::into_raw(Box::new(info));
        Ok(&mut **slot)
    }
}

pub fn has_stginfo_slot_set(tp: *mut PyTypeObject) -> bool {
    match stg_slot(tp) {
        Some(s) => unsafe { !(*s).is_null() },
        None => false,
    }
}

// ------------------------------------------------------------------ metaclass base

unsafe extern "C" fn ctype_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = o as *mut PyTypeObject;
        if let Some(slot) = stg_slot(tp) {
            let p = *slot;
            *slot = std::ptr::null_mut();
            if !p.is_null() {
                drop(Box::from_raw(p));
            }
        }
        let base_dealloc = (*rpy_runtime::types::typeobject::type_type()).tp_dealloc.unwrap();
        base_dealloc(o);
    }
}

unsafe extern "C" fn ctype_traverse(o: *mut PyObject, v: visitproc, arg: *mut std::ffi::c_void) -> c_int {
    unsafe {
        if let Some(slot) = stg_slot(o as *mut PyTypeObject) {
            let p = *slot;
            if !p.is_null() {
                let f = v.unwrap();
                let x = (*p).visit(&mut |q| f(q, arg));
                if x != 0 {
                    return x;
                }
            }
        }
        match (*rpy_runtime::types::typeobject::type_type()).tp_traverse {
            Some(t) => t(o, v, arg),
            None => 0,
        }
    }
}

unsafe extern "C" fn ctype_clear(o: *mut PyObject) -> c_int {
    unsafe {
        if let Some(slot) = stg_slot(o as *mut PyTypeObject) {
            let p = *slot;
            if !p.is_null() {
                (*p).clear();
            }
        }
        match (*rpy_runtime::types::typeobject::type_type()).tp_clear {
            Some(t) => t(o),
            None => 0,
        }
    }
}

fn ctype_sizeof(o: &PyObject) -> PyResult {
    let mut n = std::mem::size_of::<PyHeapTypeObject>() + std::mem::size_of::<*mut StgInfo>();
    if let Some(i) = stginfo_of_type(o) {
        n += std::mem::size_of::<StgInfo>();
        n += i.ffi_elements.len() * 8;
        n += i.shape.len() * 8;
        n += i.format.as_ref().map_or(0, |f| f.len() + 1);
    }
    Ok(rpy_runtime::types::int::new_int(n as i64))
}

/// `__pointer_type__` getter on ctypes types.
fn get_pointer_type(o: &PyObject) -> PyResult {
    if let Some(i) = stginfo_of_type(o) {
        if let Some(p) = &i.pointer_type {
            return Ok(p.clone());
        }
    }
    // fall back to the class dict (set before the StgInfo existed)
    unsafe {
        let tp = o.as_ptr() as *mut PyTypeObject;
        if !(*tp).tp_dict.is_null() {
            if let Some(v) = rpy_runtime::types::dict::get_item_str(&*(*tp).tp_dict, &new_str("__pointer_type__")) {
                return Ok(v);
            }
        }
    }
    let r = repr(o)?;
    raise!(AttributeError, "{} has no attribute '__pointer_type__'", r)
}

fn set_pointer_type(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match stginfo_of_type(o) {
        Some(i) => {
            i.pointer_type = v.map(|x| x.to_ref());
            Ok(())
        }
        None => {
            // no StgInfo yet: store in the type dict
            unsafe {
                let tp = o.as_ptr() as *mut PyTypeObject;
                let d = &*(*tp).tp_dict;
                let k = new_str("__pointer_type__");
                match v {
                    Some(v) => rpy_runtime::types::dict::set_item(d, &k, v)?,
                    None => {
                        if rpy_runtime::types::dict::get_item_str(d, &k).is_some() {
                            rpy_runtime::types::dict::del_item(d, &k)?;
                        }
                    }
                }
                capi::PyType_Modified(tp);
            }
            Ok(())
        }
    }
}

/// Initialise a metaclass static type deriving from CType_Type.
pub unsafe fn init_meta(t: *mut PyTypeObject, name: &'static std::ffi::CStr, base: *mut PyTypeObject) {
    unsafe {
        let t = &mut *t;
        t.tp_name = name.as_ptr();
        t.tp_basicsize = (STG_OFFSET + std::mem::size_of::<*mut StgInfo>()) as isize;
        t.tp_itemsize = (*rpy_runtime::types::typeobject::type_type()).tp_itemsize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_TYPE_SUBCLASS;
        t.tp_base = base;
        t.tp_dealloc = Some(ctype_dealloc);
        t.tp_traverse = Some(ctype_traverse);
        t.tp_clear = Some(ctype_clear);
    }
}

pub fn init_ctype_type() -> PyResult<()> {
    unsafe {
        let t = ctype_type();
        init_meta(t, c"_ctypes.CType_Type", rpy_runtime::types::typeobject::type_type());
        (*t).tp_methods = rpy_runtime::mdefs![("__sizeof__", NOARGS, ctype_sizeof)];
        (*t).tp_getset = rpy_runtime::getset_table![("__pointer_type__", get_pointer_type, set_pointer_type)];
        rpy_runtime::types::typeobject::type_ready(t)?;
    }
    Ok(())
}

/// Set the metaclass of a static ctypes base type before it is readied.
pub unsafe fn set_metaclass(t: *mut PyTypeObject, meta: *mut PyTypeObject) {
    unsafe { (*(t as *mut PyObject)).ob_type.store(meta, Relaxed) }
}

/// Raise ArgumentError-free TypeError with a type's name helper.
pub fn type_qualname_of(o: &PyObject) -> String {
    if is_type(o) { type_name(o.as_ptr() as *mut PyTypeObject) } else { tname(o) }
}

pub fn c_char_ptr(s: &str) -> CString {
    cstring(s)
}

pub fn _unused(_: *const c_char) {}
