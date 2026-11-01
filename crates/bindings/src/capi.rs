//! Declarations of the exported C API functions (defined by rpy-runtime with
//! `#[no_mangle]`) that the binding modules use. Signatures follow spec/Include.
#![allow(non_snake_case, dead_code)]

use rpy_runtime::ffi::{PyObject, PyTypeObject, Py_buffer};
use std::ffi::{c_char, c_double, c_int, c_long, c_longlong, c_uint, c_ulong, c_ulonglong, c_void};

pub type Ob = *mut PyObject;

unsafe extern "C" {
    // objects
    pub fn PyObject_GetAttrString(o: Ob, name: *const c_char) -> Ob;
    pub fn PyObject_GetAttr(o: Ob, name: Ob) -> Ob;
    pub fn PyObject_SetAttr(o: Ob, name: Ob, v: Ob) -> c_int;
    pub fn PyObject_SetAttrString(o: Ob, name: *const c_char, v: Ob) -> c_int;
    pub fn PyObject_GetOptionalAttr(o: Ob, name: Ob, result: *mut Ob) -> c_int;
    pub fn PyObject_GetOptionalAttrString(o: Ob, name: *const c_char, result: *mut Ob) -> c_int;
    pub fn PyObject_HasAttrWithError(o: Ob, name: Ob) -> c_int;
    pub fn PyObject_Call(f: Ob, args: Ob, kw: Ob) -> Ob;
    pub fn PyObject_CallObject(f: Ob, args: Ob) -> Ob;
    pub fn PyObject_CallOneArg(f: Ob, a: Ob) -> Ob;
    pub fn PyObject_CallNoArgs(f: Ob) -> Ob;
    pub fn PyObject_Vectorcall(f: Ob, args: *const Ob, nargsf: usize, kwnames: Ob) -> Ob;
    pub fn PyObject_IsInstance(o: Ob, cls: Ob) -> c_int;
    pub fn PyObject_IsSubclass(o: Ob, cls: Ob) -> c_int;
    pub fn PyObject_IsTrue(o: Ob) -> c_int;
    pub fn PyObject_Repr(o: Ob) -> Ob;
    pub fn PyObject_Str(o: Ob) -> Ob;
    pub fn PyObject_GetItem(o: Ob, k: Ob) -> Ob;
    pub fn PyObject_SetItem(o: Ob, k: Ob, v: Ob) -> c_int;
    pub fn PyObject_GetIter(o: Ob) -> Ob;
    pub fn PyObject_RichCompareBool(a: Ob, b: Ob, op: c_int) -> c_int;
    pub fn PyObject_GetBuffer(o: Ob, view: *mut Py_buffer, flags: c_int) -> c_int;
    pub fn PyBuffer_Release(view: *mut Py_buffer);
    pub fn PyObject_CheckBuffer(o: Ob) -> c_int;
    pub fn PyObject_GenericGetAttr(o: Ob, name: Ob) -> Ob;
    pub fn PyObject_GenericSetAttr(o: Ob, name: Ob, v: Ob) -> c_int;
    pub fn PyObject_Hash(o: Ob) -> isize;
    pub fn PyObject_Length(o: Ob) -> isize;
    pub fn PyIter_Next(o: Ob) -> Ob;
    pub fn PyCallable_Check(o: Ob) -> c_int;
    pub fn PyObject_Malloc(n: usize) -> *mut c_void;
    pub fn PyObject_Free(p: *mut c_void);
    pub fn PyMem_Malloc(n: usize) -> *mut c_void;
    pub fn PyMem_Calloc(n: usize, s: usize) -> *mut c_void;
    pub fn PyMem_Realloc(p: *mut c_void, n: usize) -> *mut c_void;
    pub fn PyMem_Free(p: *mut c_void);
    pub fn PyObject_ClearWeakRefs(o: Ob);

    // type
    pub fn PyType_IsSubtype(a: *mut PyTypeObject, b: *mut PyTypeObject) -> c_int;
    pub fn PyType_GenericAlloc(t: *mut PyTypeObject, n: isize) -> Ob;
    pub fn PyType_Modified(t: *mut PyTypeObject);
    pub fn PyType_GetName(t: *mut PyTypeObject) -> Ob;
    pub fn PyType_GetQualName(t: *mut PyTypeObject) -> Ob;

    // numbers
    pub fn PyLong_FromLong(v: c_long) -> Ob;
    pub fn PyLong_FromUnsignedLong(v: c_ulong) -> Ob;
    pub fn PyLong_FromLongLong(v: c_longlong) -> Ob;
    pub fn PyLong_FromUnsignedLongLong(v: c_ulonglong) -> Ob;
    pub fn PyLong_FromSsize_t(v: isize) -> Ob;
    pub fn PyLong_FromSize_t(v: usize) -> Ob;
    pub fn PyLong_FromVoidPtr(p: *mut c_void) -> Ob;
    pub fn PyLong_AsVoidPtr(o: Ob) -> *mut c_void;
    pub fn PyLong_AsLong(o: Ob) -> c_long;
    pub fn PyLong_AsLongLong(o: Ob) -> c_longlong;
    pub fn PyLong_AsUnsignedLongLong(o: Ob) -> c_ulonglong;
    pub fn PyLong_AsUnsignedLongMask(o: Ob) -> c_ulong;
    pub fn PyLong_AsUnsignedLongLongMask(o: Ob) -> c_ulonglong;
    pub fn PyLong_AsSsize_t(o: Ob) -> isize;
    pub fn PyLong_AsSize_t(o: Ob) -> usize;
    pub fn PyLong_AsDouble(o: Ob) -> c_double;
    pub fn PyLong_AsInt(o: Ob) -> c_int;
    pub fn PyLong_FromDouble(v: c_double) -> Ob;
    pub fn PyLong_AsNativeBytes(o: Ob, buf: *mut c_void, n: isize, flags: c_int) -> isize;
    pub fn PyLong_FromNativeBytes(buf: *const c_void, n: usize, flags: c_int) -> Ob;
    pub fn PyLong_FromUnsignedNativeBytes(buf: *const c_void, n: usize, flags: c_int) -> Ob;
    pub fn PyNumber_Index(o: Ob) -> Ob;
    pub fn PyIndex_Check(o: Ob) -> c_int;
    pub fn PyFloat_FromDouble(v: c_double) -> Ob;
    pub fn PyFloat_AsDouble(o: Ob) -> c_double;
    pub fn PyComplex_FromDoubles(r: c_double, i: c_double) -> Ob;
    pub fn PyComplex_RealAsDouble(o: Ob) -> c_double;
    pub fn PyComplex_ImagAsDouble(o: Ob) -> c_double;
    pub fn PyBool_FromLong(v: c_long) -> Ob;

    // bytes / str
    pub fn PyBytes_FromStringAndSize(s: *const c_char, n: isize) -> Ob;
    pub fn PyBytes_FromString(s: *const c_char) -> Ob;
    pub fn PyBytes_AsString(o: Ob) -> *mut c_char;
    pub fn PyBytes_Size(o: Ob) -> isize;
    pub fn PyBytes_AsStringAndSize(o: Ob, s: *mut *mut c_char, n: *mut isize) -> c_int;
    pub fn PyByteArray_AsString(o: Ob) -> *mut c_char;
    pub fn PyByteArray_Size(o: Ob) -> isize;
    pub fn PyUnicode_FromString(s: *const c_char) -> Ob;
    pub fn PyUnicode_FromStringAndSize(s: *const c_char, n: isize) -> Ob;
    pub fn PyUnicode_FromWideChar(w: *const i32, n: isize) -> Ob;
    pub fn PyUnicode_AsWideChar(o: Ob, w: *mut i32, n: isize) -> isize;
    pub fn PyUnicode_AsWideCharString(o: Ob, n: *mut isize) -> *mut i32;
    pub fn PyUnicode_AsUTF8AndSize(o: Ob, n: *mut isize) -> *const c_char;
    pub fn PyUnicode_AsUTF8(o: Ob) -> *const c_char;
    pub fn PyUnicode_GetLength(o: Ob) -> isize;
    pub fn PyUnicode_FromOrdinal(c: c_int) -> Ob;
    pub fn PyUnicode_ReadChar(o: Ob, i: isize) -> u32;
    pub fn PyUnicode_InternFromString(s: *const c_char) -> Ob;
    pub fn PyUnicode_DecodeFSDefault(s: *const c_char) -> Ob;
    pub fn PyUnicode_EncodeFSDefault(o: Ob) -> Ob;
    pub fn PyUnicode_FSConverter(o: Ob, out: *mut c_void) -> c_int;
    pub fn PyUnicode_CompareWithASCIIString(o: Ob, s: *const c_char) -> c_int;
    pub fn PyUnicode_AsEncodedString(o: Ob, enc: *const c_char, errors: *const c_char) -> Ob;
    pub fn PyUnicode_Decode(s: *const c_char, n: isize, enc: *const c_char, errors: *const c_char) -> Ob;
    pub fn PyUnicode_Concat(a: Ob, b: Ob) -> Ob;
    pub fn PyOS_double_to_string(v: c_double, code: c_char, prec: c_int, flags: c_int, ty: *mut c_int) -> *mut c_char;

    // containers
    pub fn PyTuple_New(n: isize) -> Ob;
    pub fn PyTuple_Size(o: Ob) -> isize;
    pub fn PyTuple_GetItem(o: Ob, i: isize) -> Ob;
    pub fn PyTuple_SetItem(o: Ob, i: isize, v: Ob) -> c_int;
    pub fn PyTuple_GetSlice(o: Ob, a: isize, b: isize) -> Ob;
    pub fn PyList_New(n: isize) -> Ob;
    pub fn PyList_Append(l: Ob, v: Ob) -> c_int;
    pub fn PyList_SetItem(l: Ob, i: isize, v: Ob) -> c_int;
    pub fn PyList_GetItem(l: Ob, i: isize) -> Ob;
    pub fn PyList_Size(l: Ob) -> isize;
    pub fn PyList_AsTuple(l: Ob) -> Ob;
    pub fn PySequence_Tuple(o: Ob) -> Ob;
    pub fn PySequence_Fast(o: Ob, msg: *const c_char) -> Ob;
    pub fn PySequence_Check(o: Ob) -> c_int;
    pub fn PySequence_Size(o: Ob) -> isize;
    pub fn PySequence_GetItem(o: Ob, i: isize) -> Ob;
    pub fn PySequence_SetItem(o: Ob, i: isize, v: Ob) -> c_int;
    pub fn PyDict_New() -> Ob;
    pub fn PyDict_SetItem(d: Ob, k: Ob, v: Ob) -> c_int;
    pub fn PyDict_SetItemString(d: Ob, k: *const c_char, v: Ob) -> c_int;
    pub fn PyDict_GetItemRef(d: Ob, k: Ob, r: *mut Ob) -> c_int;
    pub fn PyDict_GetItemStringRef(d: Ob, k: *const c_char, r: *mut Ob) -> c_int;
    pub fn PyDict_DelItem(d: Ob, k: Ob) -> c_int;
    pub fn PyDict_Next(d: Ob, pos: *mut isize, k: *mut Ob, v: *mut Ob) -> c_int;
    pub fn PyDict_Update(d: Ob, o: Ob) -> c_int;
    pub fn PyDict_Copy(d: Ob) -> Ob;
    pub fn PyDict_Size(d: Ob) -> isize;
    pub fn PyDict_Contains(d: Ob, k: Ob) -> c_int;
    pub fn PySlice_Unpack(s: Ob, start: *mut isize, stop: *mut isize, step: *mut isize) -> c_int;
    pub fn PySlice_AdjustIndices(len: isize, start: *mut isize, stop: *mut isize, step: isize) -> isize;
    pub fn PyMemoryView_FromMemory(mem: *mut c_char, size: isize, flags: c_int) -> Ob;
    pub fn PyMemoryView_FromObject(o: Ob) -> Ob;
    pub fn PyCapsule_New(p: *mut c_void, name: *const c_char, d: *mut c_void) -> Ob;

    // errors
    pub fn PyErr_SetString(t: Ob, msg: *const c_char);
    pub fn PyErr_SetObject(t: Ob, v: Ob);
    pub fn PyErr_Occurred() -> Ob;
    pub fn PyErr_ExceptionMatches(t: Ob) -> c_int;
    pub fn PyErr_Clear();
    pub fn PyErr_GetRaisedException() -> Ob;
    pub fn PyErr_SetRaisedException(e: Ob);
    pub fn PyErr_NoMemory() -> Ob;
    pub fn PyErr_SetFromErrno(t: Ob) -> Ob;
    pub fn PyErr_WriteUnraisable(o: Ob);
    pub fn PyErr_FormatUnraisable(fmt: *const c_char, ...);
    pub fn PyErr_WarnEx(cat: Ob, msg: *const c_char, level: isize) -> c_int;
    pub fn PyErr_NewException(name: *const c_char, base: Ob, d: Ob) -> Ob;
    pub fn PyErr_CheckSignals() -> c_int;
    pub fn PyException_SetCause(e: Ob, c: Ob);
    pub fn PyException_SetContext(e: Ob, c: Ob);

    // modules / misc
    pub fn PyImport_ImportModule(name: *const c_char) -> Ob;
    pub fn PyImport_ImportModuleAttrString(m: *const c_char, a: *const c_char) -> Ob;
    pub fn PySys_Audit(event: *const c_char, fmt: *const c_char, ...) -> c_int;
    pub fn PySys_GetObject(name: *const c_char) -> Ob;
    pub fn PyModule_AddObjectRef(m: Ob, name: *const c_char, v: Ob) -> c_int;
    pub fn PyModule_AddIntConstant(m: Ob, name: *const c_char, v: c_long) -> c_int;
    pub fn PyModule_AddStringConstant(m: Ob, name: *const c_char, v: *const c_char) -> c_int;
    pub fn PyModule_GetDict(m: Ob) -> Ob;

    // threads
    pub fn PyGILState_Ensure() -> c_int;
    pub fn PyGILState_Release(s: c_int);
    pub fn PyEval_SaveThread() -> *mut c_void;
    pub fn PyEval_RestoreThread(t: *mut c_void);

    // descriptors
    pub fn PyDescr_NewGetSet(t: *mut PyTypeObject, gs: *mut rpy_runtime::ffi::PyGetSetDef) -> Ob;
    pub fn PyCFunction_NewEx(ml: *mut rpy_runtime::ffi::PyMethodDef, s: Ob, m: Ob) -> Ob;
    pub fn PyMethod_New(f: Ob, s: Ob) -> Ob;
    pub fn PyWeakref_NewRef(o: Ob, cb: Ob) -> Ob;

    pub fn Py_IsInitialized() -> c_int;
}

#[allow(unused)]
pub fn _unused(_: c_uint) {}

/// Exception type objects (borrowed, immortal statics).
pub mod exc {
    use super::Ob;
    macro_rules! e {
        ($($n:ident),*) => { $( #[inline] pub fn $n() -> Ob { unsafe { rpy_runtime::types::exceptions::$n() as Ob } } )* };
    }
    e!(TypeError, ValueError, OverflowError, AttributeError, IndexError, KeyError, RuntimeError, SystemError,
       MemoryError, OSError, NotImplementedError, RuntimeWarning, DeprecationWarning, ZeroDivisionError,
       StopIteration, Exception, UnicodeEncodeError, UnicodeDecodeError, BufferError, ImportError, LookupError,
       ConnectionError, TimeoutError, UserWarning);
}
