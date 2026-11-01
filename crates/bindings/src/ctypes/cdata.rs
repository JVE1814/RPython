//! CDataObject: the instance layout shared by all ctypes instances, the `_CData`
//! base type, memory ownership/keep-alive rules, generic get/set and the methods
//! shared by all ctypes metaclasses (from_address, from_buffer, in_dll, `*`).

use super::*;
use rpy_runtime::types::{dict, tuple};
use std::ffi::c_void;

#[repr(C)]
pub struct CDataObject {
    pub ob_base: PyObject,
    pub b_ptr: *mut u8,
    pub b_needsfree: c_int,
    pub b_base: *mut PyObject,
    pub b_size: isize,
    pub b_length: isize,
    pub b_index: isize,
    pub b_objects: *mut PyObject,
    pub b_weakreflist: *mut PyObject,
    pub b_value: [u128; 1],
}

pub const INLINE_SIZE: usize = 16;

#[inline]
pub fn cd(o: &PyObject) -> &'static mut CDataObject {
    unsafe { &mut *(o.as_ptr() as *mut CDataObject) }
}

#[inline]
pub fn cdp(o: *mut PyObject) -> &'static mut CDataObject {
    unsafe { &mut *(o as *mut CDataObject) }
}

pub static mut CDATA_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn cdata_type() -> *mut PyTypeObject {
    &raw mut CDATA_TYPE
}

pub fn is_cdata(o: &PyObject) -> bool {
    subtype(o.type_ptr(), cdata_type())
}

// ------------------------------------------------------------------ allocation

/// Allocate an uninitialised instance (b_ptr not set) of `tp`.
pub fn alloc_instance(tp: *mut PyTypeObject) -> PyResult {
    unsafe {
        let a = (*tp).tp_alloc.unwrap_or(rpy_runtime::types::typeobject::generic_alloc);
        let op = a(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let c = cdp(op);
        c.b_ptr = std::ptr::null_mut();
        c.b_needsfree = 0;
        c.b_base = std::ptr::null_mut();
        c.b_objects = std::ptr::null_mut();
        c.b_index = 0;
        c.b_size = 0;
        c.b_length = 0;
        Ok(PyRef::from_raw(op))
    }
}

/// Give `obj` its own zeroed memory of `size` bytes.
pub fn malloc_buffer(obj: &PyObject, size: isize) -> PyResult<()> {
    let c = cd(obj);
    let size = size.max(0) as usize;
    if size <= INLINE_SIZE {
        c.b_ptr = c.b_value.as_mut_ptr() as *mut u8;
        unsafe { std::ptr::write_bytes(c.b_ptr, 0, INLINE_SIZE) };
    } else {
        let p = unsafe { libc::calloc(1, size) as *mut u8 };
        if p.is_null() {
            unsafe { capi::PyErr_NoMemory() };
            return Err(PyErr);
        }
        c.b_ptr = p;
    }
    c.b_needsfree = 1;
    c.b_size = size as isize;
    Ok(())
}

/// `tp()` without calling __init__: zeroed memory of the type's size.
pub fn generic_new(tp: *mut PyTypeObject) -> PyResult {
    let info = match stginfo(tp) {
        Some(i) => i,
        None => raise!(TypeError, "abstract class"),
    };
    let size = info.size;
    let length = info.length;
    info.dict_flags |= DICTFLAG_FINAL;
    let obj = alloc_instance(tp)?;
    malloc_buffer(&obj, size)?;
    let c = cd(&obj);
    c.b_size = size;
    c.b_length = length;
    Ok(obj)
}

pub unsafe extern "C" fn generic_tp_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    into_c(generic_new(tp))
}

/// An instance of `tp` sharing memory at `adr` owned by `base` (or a copy if no base).
pub fn from_base_obj(tp: *mut PyTypeObject, base: Option<&PyObject>, index: isize, adr: *mut u8) -> PyResult {
    let info = match stginfo(tp) {
        Some(i) => i,
        None => raise!(TypeError, "abstract class"),
    };
    let obj = alloc_instance(tp)?;
    let c = cd(&obj);
    c.b_index = index;
    c.b_length = info.length;
    c.b_size = info.size;
    match base {
        Some(b) => {
            c.b_ptr = adr;
            c.b_needsfree = 0;
            c.b_base = b.to_ref().into_raw();
        }
        None => {
            malloc_buffer(&obj, info.size)?;
            if !adr.is_null() && info.size > 0 {
                unsafe { std::ptr::copy(adr, c.b_ptr, info.size as usize) };
            }
        }
    }
    Ok(obj)
}

/// An instance of `tp` viewing memory at `adr` (not owned).
pub fn at_address(tp: *mut PyTypeObject, adr: *mut u8) -> PyResult {
    let info = match stginfo(tp) {
        Some(i) => i,
        None => raise!(TypeError, "abstract class"),
    };
    let obj = alloc_instance(tp)?;
    let c = cd(&obj);
    c.b_ptr = adr;
    c.b_length = info.length;
    c.b_size = info.size;
    Ok(obj)
}

unsafe extern "C" fn cdata_dealloc(op: *mut PyObject) {
    unsafe {
        rpy_runtime::gc::untrack(op);
        let c = cdp(op);
        if !c.b_weakreflist.is_null() {
            capi::PyObject_ClearWeakRefs(op);
        }
        cdata_clear_impl(c);
        if c.b_needsfree != 0 && !c.b_ptr.is_null() && c.b_ptr != c.b_value.as_mut_ptr() as *mut u8 {
            libc::free(c.b_ptr as *mut c_void);
        }
        c.b_ptr = std::ptr::null_mut();
        let tp = ob_type(op);
        rpy_runtime::types::typeobject::generic_free_and_type_decref(op, tp);
    }
}

unsafe fn cdata_clear_impl(c: &mut CDataObject) {
    unsafe {
        let a = c.b_objects;
        c.b_objects = std::ptr::null_mut();
        xdecref(a);
        let b = c.b_base;
        c.b_base = std::ptr::null_mut();
        xdecref(b);
    }
}

unsafe extern "C" fn cdata_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let c = cdp(op);
        let a = c.b_objects;
        c.b_objects = std::ptr::null_mut();
        xdecref(a);
    }
    0
}

unsafe extern "C" fn cdata_traverse(op: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let c = cdp(op);
        let f = v.unwrap();
        for p in [c.b_objects, c.b_base] {
            if !p.is_null() {
                let r = f(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

// ------------------------------------------------------------------ keep-alive

/// The object at the root of the memory-sharing chain; creates its container.
pub fn get_container(o: &PyObject) -> *mut PyObject {
    let mut p = o.as_ptr();
    unsafe {
        while !cdp(p).b_base.is_null() {
            p = cdp(p).b_base;
        }
        let c = cdp(p);
        if c.b_objects.is_null() {
            if c.b_length != 0 {
                c.b_objects = rpy_runtime::types::dict::new_dict().into_raw();
            } else {
                c.b_objects = none().into_raw();
            }
        }
    }
    p
}

/// The keep-alive objects of `o` (its container's b_objects).
pub fn get_kept_objects(o: &PyObject) -> PyRef {
    let p = get_container(o);
    unsafe { PyRef::from_borrowed(cdp(p).b_objects) }
}

fn unique_key(target: &PyObject, index: isize) -> String {
    let mut s = format!("{:x}", index);
    let mut p = target.as_ptr();
    unsafe {
        while !cdp(p).b_base.is_null() {
            s.push_str(&format!(":{:x}", cdp(p).b_index));
            p = cdp(p).b_base;
        }
    }
    s
}

/// Keep `keep` alive as long as `target`'s memory lives.
pub fn keep_ref(target: &PyObject, index: isize, keep: Option<PyRef>) -> PyResult<()> {
    let keep = match keep {
        None => return Ok(()),
        Some(k) if is_none(&k) => return Ok(()),
        Some(k) => k,
    };
    let root = get_container(target);
    unsafe {
        let c = cdp(root);
        if c.b_objects.is_null() || !is_dict_exact(&*c.b_objects) {
            let old = c.b_objects;
            c.b_objects = keep.into_raw();
            xdecref(old);
            return Ok(());
        }
        let key = new_str(&unique_key(target, index));
        dict::set_item(&*c.b_objects, &key, &keep)
    }
}

// ------------------------------------------------------------------ generic get / set

/// Is `tp` a subclass of a simple type (not a direct `_SimpleCData` child)?
pub fn simple_instance(tp: *mut PyTypeObject) -> bool {
    unsafe {
        if subtype(ob_type(tp as *mut PyObject), simple::simple_meta()) {
            return (*tp).tp_base != simple::simple_type();
        }
    }
    false
}

/// Field getter override (char arrays inside structures).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FieldCode {
    None,
    Code(u8),
}

/// Read a value of type `tp` at `adr` inside `src`.
pub fn cdata_get(tp: &PyObject, code: FieldCode, src: Option<&PyObject>, index: isize, size: isize, bits: Option<(u32, u32)>, adr: *mut u8) -> PyResult {
    if let FieldCode::Code(c) = code {
        return simple::get(c, false, adr, size, bits);
    }
    let tpp = tp.as_ptr() as *mut PyTypeObject;
    if let Some(info) = stginfo_of_type(tp) {
        if info.kind == Kind::Simple && !simple_instance(tpp) {
            return simple::get(info.code, info.swapped, adr, size, bits);
        }
    }
    from_base_obj(tpp, src, index, adr)
}

/// Store `value` into memory `ptr` of type `tp`; returns the object to keep alive.
pub fn cdata_set_raw(tp: &PyObject, code: FieldCode, value: &PyObject, size: isize, bits: Option<(u32, u32)>, ptr: *mut u8) -> PyResult<Option<PyRef>> {
    if let FieldCode::Code(c) = code {
        return simple::set(c, false, ptr, value, size, bits);
    }
    let info = stginfo_of_type(tp);
    if !is_cdata(value) {
        if let Some(i) = &info {
            if i.kind == Kind::Simple {
                return simple::set(i.code, i.swapped, ptr, value, size, bits);
            }
        }
        if is_tuple(value) {
            let ob = match rpy_runtime::abstract_::call_tp_call(tp, value, None) {
                Ok(o) => o,
                Err(_) => return Err(extend_error_runtime(&type_name(tp.as_ptr() as *mut PyTypeObject))),
            };
            return cdata_set_raw(tp, code, &ob, size, bits, ptr);
        }
        if is_none(value) && info.as_ref().map_or(false, |i| i.kind == Kind::Pointer) {
            unsafe { *(ptr as *mut *mut u8) = std::ptr::null_mut() };
            return Ok(None);
        }
        raise!(TypeError, "expected {} instance, got {}", type_name(tp.as_ptr() as *mut PyTypeObject), tname(value));
    }
    let src = cd(value);
    if isinstance(value, tp)? {
        if size > 0 && !src.b_ptr.is_null() {
            unsafe { std::ptr::copy(src.b_ptr, ptr, size as usize) };
        }
        return Ok(Some(get_kept_objects(value)));
    }
    if let Some(i) = &info {
        if i.kind == Kind::Pointer {
            if let Some(vi) = stginfo_of_obj(value) {
                if vi.kind == Kind::Array {
                    let same = match (&vi.proto, &i.proto) {
                        (Some(a), Some(b)) => a.is(b),
                        _ => false,
                    };
                    if !same {
                        raise!(
                            TypeError,
                            "incompatible types, {} instance instead of {} instance",
                            tname(value),
                            type_name(tp.as_ptr() as *mut PyTypeObject)
                        );
                    }
                    unsafe { *(ptr as *mut *mut u8) = src.b_ptr };
                    let keep = get_kept_objects(value);
                    return Ok(Some(tuple::new_tuple(vec![keep, value.to_ref()])));
                }
            }
        }
    }
    raise!(TypeError, "incompatible types, {} instance instead of {} instance", tname(value), type_name(tp.as_ptr() as *mut PyTypeObject))
}

/// Replace the current exception by `RuntimeError("(prefix) ExcType: msg")`.
pub fn extend_error_runtime(prefix: &str) -> PyErr {
    let e = match rpy_runtime::errors::take_exc() {
        Some(e) => e,
        None => return PyErr,
    };
    let tn = type_name(e.type_ptr());
    let msg = rpy_runtime::abstract_::str_string(&e).unwrap_or_default();
    set_err(capi::exc::RuntimeError(), &format!("({}) {}: {}", prefix, tn, msg))
}

/// Store and record the keep-alive object under `index` of `dst`.
pub fn cdata_set(dst: &PyObject, tp: &PyObject, code: FieldCode, value: &PyObject, index: isize, size: isize, bits: Option<(u32, u32)>, ptr: *mut u8) -> PyResult<()> {
    let keep = cdata_set_raw(tp, code, value, size, bits, ptr)?;
    keep_ref(dst, index, keep)
}

// ------------------------------------------------------------------ _CData methods

fn b_base_get(o: &PyObject) -> PyResult {
    let c = cd(o);
    if c.b_base.is_null() { Ok(none()) } else { Ok(unsafe { PyRef::from_borrowed(c.b_base) }) }
}

fn b_needsfree_get(o: &PyObject) -> PyResult {
    Ok(new_int(cd(o).b_needsfree as i64))
}

fn objects_get(o: &PyObject) -> PyResult {
    let c = cd(o);
    if c.b_objects.is_null() { Ok(none()) } else { Ok(unsafe { PyRef::from_borrowed(c.b_objects) }) }
}

fn cdata_reduce(o: &PyObject) -> PyResult {
    let info = match stginfo_of_obj(o) {
        Some(i) => i,
        None => raise!(TypeError, "abstract class"),
    };
    if info.dict_flags & (TYPEFLAG_ISPOINTER | TYPEFLAG_HASPOINTER) != 0 {
        raise!(ValueError, "ctypes objects containing pointers cannot be pickled");
    }
    let d = match rpy_runtime::types::typeobject::instance_dict(o) {
        Some(d) => d,
        None => rpy_runtime::types::dict::new_dict(),
    };
    let c = cd(o);
    let data = rpy_runtime::types::bytes::new_bytes(unsafe { std::slice::from_raw_parts(c.b_ptr, c.b_size.max(0) as usize) });
    let unpickle = module::module_attr("_unpickle")?;
    let tp = rpy_runtime::types::typeobject::type_ref(o.type_ptr());
    let state = tuple::new_tuple(vec![d, data]);
    Ok(tuple::new_tuple(vec![unpickle, tuple::new_tuple(vec![tp, state])]))
}

fn cdata_setstate(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 || !dict::is_dict(args[0]) || !is_bytes(args[1]) {
        raise!(TypeError, "__setstate__ expects (dict, bytes)");
    }
    let data = rpy_runtime::types::bytes::bytes_data(args[1]);
    let c = cd(o);
    let n = data.len().min(c.b_size.max(0) as usize);
    unsafe { std::ptr::copy(data.as_ptr(), c.b_ptr, n) };
    let mydict = getattr(o, "__dict__")?;
    if !dict::is_dict(&mydict) {
        raise!(TypeError, "{}.__dict__ must be a dictionary, not {}", tname(o), tname(&mydict));
    }
    unsafe {
        if capi::PyDict_Update(mydict.as_ptr(), args[0].as_ptr()) < 0 {
            return Err(PyErr);
        }
    }
    Ok(none())
}

fn cdata_from_outparam(o: &PyObject) -> PyResult {
    Ok(o.to_ref())
}

// ------------------------------------------------------------------ buffer protocol

struct BufInternal {
    format: CString,
    shape: Vec<isize>,
}

unsafe extern "C" fn cdata_getbuffer(op: *mut PyObject, view: *mut Py_buffer, _flags: c_int) -> c_int {
    unsafe {
        let o = &*op;
        let c = cd(o);
        let info = stginfo_of_obj(o);
        // innermost item type of nested arrays
        let mut item_info = info.as_deref();
        while let Some(i) = item_info {
            if i.kind == Kind::Array {
                item_info = i.proto.as_ref().and_then(|p| stginfo_of_type(p)).map(|x| &*x);
            } else {
                break;
            }
        }
        let fmt = info.as_ref().and_then(|i| i.format.clone()).unwrap_or_else(|| "B".to_string());
        let shape = info.as_ref().map(|i| i.shape.clone()).unwrap_or_default();
        let internal = Box::new(BufInternal { format: CString::new(fmt).unwrap(), shape });
        let v = &mut *view;
        v.buf = c.b_ptr as *mut c_void;
        v.obj = o.to_ref().into_raw();
        v.len = c.b_size;
        v.readonly = 0;
        v.format = internal.format.as_ptr() as *mut c_char;
        v.ndim = internal.shape.len() as c_int;
        v.shape = if internal.shape.is_empty() { std::ptr::null_mut() } else { internal.shape.as_ptr() as *mut isize };
        v.itemsize = match item_info {
            Some(i) => i.size,
            None => c.b_size,
        };
        v.strides = std::ptr::null_mut();
        v.suboffsets = std::ptr::null_mut();
        v.internal = Box::into_raw(internal) as *mut c_void;
        0
    }
}

unsafe extern "C" fn cdata_releasebuffer(_op: *mut PyObject, view: *mut Py_buffer) {
    unsafe {
        let v = &mut *view;
        if !v.internal.is_null() {
            drop(Box::from_raw(v.internal as *mut BufInternal));
            v.internal = std::ptr::null_mut();
        }
    }
}

static mut CDATA_BUFFER: PyBufferProcs = PyBufferProcs { bf_getbuffer: Some(cdata_getbuffer), bf_releasebuffer: Some(cdata_releasebuffer) };

pub fn init_cdata_type() -> PyResult<()> {
    unsafe {
        let t = cdata_type();
        (*t).tp_name = c"_ctypes._CData".as_ptr();
        (*t).tp_basicsize = std::mem::size_of::<CDataObject>() as isize;
        (*t).tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC;
        (*t).tp_dealloc = Some(cdata_dealloc);
        (*t).tp_traverse = Some(cdata_traverse);
        (*t).tp_clear = Some(cdata_clear);
        (*t).tp_weaklistoffset = std::mem::offset_of!(CDataObject, b_weakreflist) as isize;
        (*t).tp_as_buffer = &raw mut CDATA_BUFFER;
        (*t).tp_alloc = Some(rpy_runtime::types::typeobject::generic_alloc);
        (*t).tp_free = Some(rpy_runtime::types::typeobject::generic_free);
        (*t).tp_methods = rpy_runtime::mdefs![
            ("__ctypes_from_outparam__", NOARGS, cdata_from_outparam),
            ("__reduce__", NOARGS, cdata_reduce),
            ("__setstate__", FAST, cdata_setstate),
        ];
        (*t).tp_getset = rpy_runtime::getset_table![
            ("_b_base_", b_base_get),
            ("_b_needsfree_", b_needsfree_get),
            ("_objects", objects_get),
        ];
        rpy_runtime::types::typeobject::type_ready(t)?;
    }
    Ok(())
}

/// Common setup for the static base classes (Structure, Array, ...).
pub unsafe fn init_base_type(t: *mut PyTypeObject, name: &'static std::ffi::CStr, meta: *mut PyTypeObject, size: usize) {
    unsafe {
        set_metaclass(t, meta);
        (*t).tp_name = name.as_ptr();
        (*t).tp_basicsize = size as isize;
        (*t).tp_base = cdata_type();
        (*t).tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC;
        (*t).tp_new = Some(generic_tp_new);
        (*t).tp_alloc = Some(rpy_runtime::types::typeobject::generic_alloc);
        (*t).tp_free = Some(rpy_runtime::types::typeobject::generic_free);
    }
}

// ------------------------------------------------------------------ metaclass methods

fn check_type_with_info(cls: &PyObject) -> PyResult<&'static mut StgInfo> {
    match stginfo_of_type(cls) {
        Some(i) => Ok(i),
        None => raise!(TypeError, "abstract class"),
    }
}

pub fn ct_from_address(cls: &PyObject, value: &PyObject) -> PyResult {
    check_type_with_info(cls)?;
    let addr = unsafe { capi::PyLong_AsVoidPtr(value.as_ptr()) };
    if addr.is_null() && err_occurred() {
        rpy_runtime::errors::clear_exc();
        raise!(TypeError, "integer expected");
    }
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.cdata"), cs!("n"), addr as isize) < 0 {
            return Err(PyErr);
        }
    }
    at_address(cls.as_ptr() as *mut PyTypeObject, addr as *mut u8)
}

fn buffer_args(fname: &str, args: &[&PyObject]) -> PyResult<(PyRef, isize)> {
    if args.is_empty() || args.len() > 2 {
        raise!(TypeError, "{}() takes 1 or 2 arguments ({} given)", fname, args.len());
    }
    let off = if args.len() == 2 { ssize(args[1])? } else { 0 };
    Ok((args[0].to_ref(), off))
}

pub fn ct_from_buffer(cls: &PyObject, args: &[&PyObject]) -> PyResult {
    let info = check_type_with_info(cls)?;
    let (obj, offset) = buffer_args("from_buffer", args)?;
    let mut view: Py_buffer = unsafe { std::mem::zeroed() };
    unsafe {
        // PyBUF_FULL_RO: accept anything, then check writability/contiguity ourselves
        if capi::PyObject_GetBuffer(obj.as_ptr(), &mut view, 0x11c) < 0 {
            return Err(PyErr);
        }
    }
    struct Rel(Py_buffer);
    impl Drop for Rel {
        fn drop(&mut self) {
            unsafe { capi::PyBuffer_Release(&mut self.0) };
        }
    }
    let view = Rel(view);
    if view.0.readonly != 0 {
        raise!(TypeError, "underlying buffer is not writable");
    }
    if !buffer_is_c_contiguous(&view.0) {
        raise!(TypeError, "underlying buffer is not C contiguous");
    }
    if offset < 0 {
        raise!(ValueError, "offset cannot be negative");
    }
    if info.size > view.0.len - offset {
        raise!(ValueError, "Buffer size too small ({} instead of at least {} bytes)", view.0.len, info.size + offset);
    }
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.cdata/buffer"), cs!("nnn"), view.0.buf as isize, view.0.len, offset) < 0 {
            return Err(PyErr);
        }
    }
    let res = at_address(cls.as_ptr() as *mut PyTypeObject, unsafe { (view.0.buf as *mut u8).offset(offset) })?;
    // keep the exporting object alive through a memoryview
    let mv = from_c(unsafe { capi::PyMemoryView_FromObject(obj.as_ptr()) })?;
    keep_ref(&res, -1, Some(mv))?;
    Ok(res)
}

fn buffer_is_c_contiguous(v: &Py_buffer) -> bool {
    if v.strides.is_null() || v.ndim == 0 {
        return true;
    }
    let mut expect = v.itemsize;
    for i in (0..v.ndim as usize).rev() {
        let sh = unsafe { *v.shape.add(i) };
        let st = unsafe { *v.strides.add(i) };
        if sh > 1 && st != expect {
            return false;
        }
        expect *= sh;
    }
    true
}

pub fn ct_from_buffer_copy(cls: &PyObject, args: &[&PyObject]) -> PyResult {
    let info = check_type_with_info(cls)?;
    let (obj, offset) = buffer_args("from_buffer_copy", args)?;
    let mut view: Py_buffer = unsafe { std::mem::zeroed() };
    unsafe {
        if capi::PyObject_GetBuffer(obj.as_ptr(), &mut view, 0) < 0 {
            return Err(PyErr);
        }
    }
    let len = view.len;
    let buf = view.buf as *mut u8;
    let r = (|| {
        if offset < 0 {
            raise!(ValueError, "offset cannot be negative");
        }
        if info.size > len - offset {
            raise!(ValueError, "Buffer size too small ({} instead of at least {} bytes)", len, info.size + offset);
        }
        unsafe {
            if capi::PySys_Audit(cs!("ctypes.cdata/buffer"), cs!("nnn"), buf as isize, len, offset) < 0 {
                return Err(PyErr);
            }
        }
        let res = generic_new(cls.as_ptr() as *mut PyTypeObject)?;
        if info.size > 0 {
            unsafe { std::ptr::copy(buf.offset(offset), cd(&res).b_ptr, info.size as usize) };
        }
        Ok(res)
    })();
    unsafe { capi::PyBuffer_Release(&mut view) };
    r
}

pub fn ct_in_dll(cls: &PyObject, args: &[&PyObject]) -> PyResult {
    check_type_with_info(cls)?;
    if args.len() != 2 {
        raise!(TypeError, "in_dll expected 2 arguments, got {}", args.len());
    }
    let dll = args[0];
    let name = args[1];
    if !is_str(name) {
        raise!(TypeError, "in_dll() argument 2 must be str, not {}", tname(name));
    }
    let handle_obj = getattr(dll, "_handle")?;
    if !is_int(&handle_obj) {
        raise!(TypeError, "the _handle attribute of the second argument must be an integer");
    }
    let handle = unsafe { capi::PyLong_AsVoidPtr(handle_obj.as_ptr()) };
    if handle.is_null() && err_occurred() {
        raise!(ValueError, "could not convert the _handle attribute to a pointer");
    }
    let nm = str_of(name);
    let cn = cstring(&nm);
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.dlsym/handle"), cs!("O"), name.as_ptr()) < 0 {
            return Err(PyErr);
        }
        libc::dlerror();
        let addr = libc::dlsym(handle, cn.as_ptr());
        if addr.is_null() {
            let e = crate::dl::dlerror_string();
            if e.is_empty() {
                raise!(ValueError, "symbol '{}' not found", nm);
            }
            raise!(ValueError, "{}", e);
        }
        at_address(cls.as_ptr() as *mut PyTypeObject, addr as *mut u8)
    }
}

/// `type * n` and `n * type`: array type creation.
pub unsafe extern "C" fn ct_multiply(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (tp, n) = if is_type(&*a) && subtype(ob_type(a), ctype_type()) {
            (a, b)
        } else if is_type(&*b) && subtype(ob_type(b), ctype_type()) {
            (b, a)
        } else {
            return rpy_runtime::types::none::not_implemented().to_ref().into_raw();
        };
        if !rpy_runtime::abstract_::has_index(&*n) {
            return rpy_runtime::types::none::not_implemented().to_ref().into_raw();
        }
        let len = match ssize(&*n) {
            Ok(v) => v,
            Err(_) => return std::ptr::null_mut(),
        };
        if len < 0 {
            let _ = err!(ValueError, "Array length must be >= 0, not {}", len);
            return std::ptr::null_mut();
        }
        into_c(array::array_type_from_ctype(&*tp, len))
    }
}

pub unsafe extern "C" fn ct_repeat(a: *mut PyObject, n: isize) -> *mut PyObject {
    if n < 0 {
        let _ = err!(ValueError, "Array length must be >= 0, not {}", n);
        return std::ptr::null_mut();
    }
    unsafe { into_c(array::array_type_from_ctype(&*a, n)) }
}

/// Generic `from_param` of structures, unions, arrays and function pointers.
pub fn ct_from_param(cls: &PyObject, value: &PyObject) -> PyResult {
    if isinstance(value, cls)? {
        return Ok(value.to_ref());
    }
    if funcptr::is_carg(value) {
        let a = funcptr::carg(value);
        if !a.obj.is_null() {
            if isinstance(r(a.obj), cls)? {
                return Ok(value.to_ref());
            }
            // byref(x) where a pointer to x's type is expected
            if let Some(info) = stginfo_of_type(cls) {
                if let Some(proto) = &info.proto {
                    if info.kind == Kind::Pointer && isinstance(r(a.obj), proto)? {
                        return Ok(value.to_ref());
                    }
                }
            }
        }
    }
    if let Some(info) = stginfo_of_type(cls) {
        if info.kind == Kind::Pointer {
            if let Some(proto) = &info.proto {
                if isinstance(value, proto)? {
                    return funcptr::byref_impl(value, 0);
                }
            }
        }
    }
    as_parameter_fallback(cls, value, |v| {
        raise!(TypeError, "expected {} instance instead of {}", type_name(cls.as_ptr() as *mut PyTypeObject), tname(v))
    })
}

/// Try `value._as_parameter_` with recursion protection.
pub fn as_parameter_fallback(cls: &PyObject, value: &PyObject, fail: impl FnOnce(&PyObject) -> PyResult) -> PyResult {
    match getattr_opt(value, "_as_parameter_")? {
        Some(asp) => rpy_runtime::modules::fncommon::with_recursion(" while processing _as_parameter_", || {
            rpy_runtime::abstract_::call_method_str(cls, "from_param", &[&asp])
        }),
        None => fail(value),
    }
}

/// Methods shared by every ctypes metaclass (with a per-kind from_param).
pub fn meta_methods(fp: *mut PyMethodDef, extra: Vec<PyMethodDef>) -> *mut PyMethodDef {
    let mut v: Vec<PyMethodDef> = Vec::new();
    unsafe {
        let base = rpy_runtime::mdefs![
            ("from_address", O, ct_from_address),
            ("from_buffer", FAST, ct_from_buffer),
            ("from_buffer_copy", FAST, ct_from_buffer_copy),
            ("in_dll", FAST, ct_in_dll),
        ];
        let mut p = base;
        while !(*p).ml_name.is_null() {
            v.push(std::ptr::read(p));
            p = p.add(1);
        }
        v.push(std::ptr::read(fp));
    }
    v.extend(extra);
    rpy_runtime::methods::table(v)
}

pub static mut META_AS_NUMBER: PyNumberMethods = unsafe { std::mem::zeroed() };
pub static mut META_AS_SEQUENCE: PySequenceMethods = unsafe { std::mem::zeroed() };

pub unsafe fn set_meta_number(t: *mut PyTypeObject) {
    unsafe {
        META_AS_SEQUENCE.sq_repeat = Some(ct_repeat);
        (*t).tp_as_sequence = &raw mut META_AS_SEQUENCE;
    }
}
