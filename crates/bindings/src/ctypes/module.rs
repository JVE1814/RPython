//! The `_ctypes` module object: functions and constants.

use super::cdata::*;
use super::*;
use rpy_runtime::types::{bytes, tuple};
use std::ffi::c_void;

static mut ARGUMENT_ERROR: *mut PyObject = std::ptr::null_mut();
static mut MODULE: *mut PyObject = std::ptr::null_mut();

pub fn argument_error_type() -> *mut PyObject {
    unsafe { ARGUMENT_ERROR }
}

pub fn module_attr(name: &str) -> PyResult {
    unsafe {
        if MODULE.is_null() {
            raise!(SystemError, "_ctypes not initialized");
        }
        getattr(&*MODULE, name)
    }
}

fn sizeof(_m: &PyObject, o: &PyObject) -> PyResult {
    if let Some(i) = stginfo_any(o) {
        if is_type(o) {
            return Ok(new_int(i.size as i64));
        }
    }
    if !is_type(o) && is_cdata(o) {
        return Ok(new_int(cd(o).b_size as i64));
    }
    raise!(TypeError, "this type has no size")
}

fn alignment(_m: &PyObject, o: &PyObject) -> PyResult {
    match stginfo_any(o) {
        Some(i) => Ok(new_int(i.align as i64)),
        None => raise!(TypeError, "no alignment info"),
    }
}

fn addressof(_m: &PyObject, o: &PyObject) -> PyResult {
    if !is_cdata(o) {
        raise!(TypeError, "invalid type");
    }
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.addressof"), cs!("(O)"), o.as_ptr()) < 0 {
            return Err(PyErr);
        }
    }
    Ok(new_ptr_int(cd(o).b_ptr as usize))
}

fn byref(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 2 {
        raise!(TypeError, "byref expected at most 2 arguments, got {}", args.len());
    }
    let off = if args.len() == 2 { ssize(args[1])? } else { 0 };
    funcptr::byref_impl(args[0], off)
}

fn resize(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 {
        raise!(TypeError, "resize expected 2 arguments, got {}", args.len());
    }
    let o = args[0];
    if !is_cdata(o) {
        raise!(TypeError, "expected ctypes instance");
    }
    let size = ssize(args[1])?;
    let info = match stginfo_of_obj(o) {
        Some(i) => i,
        None => raise!(TypeError, "expected ctypes instance"),
    };
    if size < info.size {
        raise!(ValueError, "minimum size is {}", info.size);
    }
    let c = cd(o);
    if c.b_needsfree == 0 {
        raise!(ValueError, "Memory cannot be resized because this object doesn't own it");
    }
    let inline = c.b_value.as_mut_ptr() as *mut u8;
    if size as usize <= INLINE_SIZE {
        c.b_size = size;
        return Ok(none());
    }
    unsafe {
        if c.b_ptr == inline {
            let p = libc::calloc(1, size as usize) as *mut u8;
            if p.is_null() {
                capi::PyErr_NoMemory();
                return Err(PyErr);
            }
            std::ptr::copy_nonoverlapping(inline, p, c.b_size.max(0) as usize);
            c.b_ptr = p;
        } else {
            let p = libc::realloc(c.b_ptr as *mut c_void, size as usize) as *mut u8;
            if p.is_null() {
                capi::PyErr_NoMemory();
                return Err(PyErr);
            }
            if size > c.b_size {
                std::ptr::write_bytes(p.offset(c.b_size), 0, (size - c.b_size) as usize);
            }
            c.b_ptr = p;
        }
    }
    c.b_size = size;
    Ok(none())
}

fn buffer_info(_m: &PyObject, o: &PyObject) -> PyResult {
    let info = match stginfo_any(o) {
        Some(i) => i,
        None => raise!(TypeError, "not a ctypes type or object"),
    };
    let fmt = info.format.clone().unwrap_or_else(|| "B".to_string());
    let shape = tuple::new_tuple(info.shape.iter().map(|&x| new_int(x as i64)).collect());
    Ok(tuple::new_tuple(vec![new_str(&fmt), new_int(info.shape.len() as i64), shape]))
}

fn get_errno(_m: &PyObject) -> PyResult {
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.get_errno"), std::ptr::null()) < 0 {
            return Err(PyErr);
        }
    }
    Ok(new_int(funcptr::CTYPES_ERRNO.with(|c| c.get()) as i64))
}

fn set_errno(_m: &PyObject, v: &PyObject) -> PyResult {
    let n = unsafe { capi::PyLong_AsInt(v.as_ptr()) };
    if n == -1 && err_occurred() {
        return Err(PyErr);
    }
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.set_errno"), cs!("i"), n) < 0 {
            return Err(PyErr);
        }
    }
    let old = funcptr::CTYPES_ERRNO.with(|c| c.replace(n));
    Ok(new_int(old as i64))
}

fn py_dlopen(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 2 {
        raise!(TypeError, "dlopen expected 1 or 2 arguments");
    }
    let mut mode = if args.len() == 2 { unsafe { capi::PyLong_AsInt(args[1].as_ptr()) } } else { libc::RTLD_NOW | libc::RTLD_LOCAL };
    if mode == -1 && err_occurred() {
        return Err(PyErr);
    }
    mode |= libc::RTLD_NOW;
    let name = args[0];
    let cname: Option<std::ffi::CString> = if is_none(name) {
        None
    } else {
        let b = from_c(unsafe { capi::PyUnicode_EncodeFSDefault(rpy_runtime::modules::posix::path::fspath(name)?.as_ptr()) })?;
        Some(std::ffi::CString::new(bytes::bytes_data(&b).to_vec()).map_err(|_| err!(ValueError, "embedded null byte"))?)
    };
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.dlopen"), cs!("O"), name.as_ptr()) < 0 {
            return Err(PyErr);
        }
    }
    let h = rpy_runtime::threads::blocking(|| unsafe { libc::dlopen(cname.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()), mode) });
    if h.is_null() {
        let e = crate::dl::dlerror_string();
        let e = if e.is_empty() { "dlopen() error".to_string() } else { e };
        return Err(set_err(capi::exc::OSError(), &e));
    }
    Ok(new_ptr_int(h as usize))
}

fn py_dlclose(_m: &PyObject, h: &PyObject) -> PyResult {
    let p = unsafe { capi::PyLong_AsVoidPtr(h.as_ptr()) };
    if p.is_null() && err_occurred() {
        return Err(PyErr);
    }
    if unsafe { libc::dlclose(p) } != 0 {
        let e = crate::dl::dlerror_string();
        return Err(set_err(capi::exc::OSError(), &e));
    }
    Ok(none())
}

fn py_dlsym(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 {
        raise!(TypeError, "dlsym expected 2 arguments");
    }
    let h = unsafe { capi::PyLong_AsVoidPtr(args[0].as_ptr()) };
    if h.is_null() && err_occurred() {
        return Err(PyErr);
    }
    if !is_str(args[1]) {
        raise!(TypeError, "dlsym() argument 2 must be str, not {}", tname(args[1]));
    }
    let nm = str_of(args[1]);
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.dlsym/handle"), cs!("O"), args[1].as_ptr()) < 0 {
            return Err(PyErr);
        }
    }
    let c = cstring(&nm);
    let p = unsafe {
        libc::dlerror();
        libc::dlsym(h, c.as_ptr())
    };
    if p.is_null() {
        let e = crate::dl::dlerror_string();
        if e.is_empty() {
            return Err(set_err(capi::exc::OSError(), &format!("symbol '{}' not found", nm)));
        }
        return Err(set_err(capi::exc::OSError(), &e));
    }
    Ok(new_ptr_int(p as usize))
}

fn call_function_impl(args: &[&PyObject], flags: i32) -> PyResult {
    if args.len() != 2 || !is_tuple(args[1]) {
        raise!(TypeError, "call_function expects (address, tuple)");
    }
    let addr = unsafe { capi::PyLong_AsVoidPtr(args[0].as_ptr()) };
    if addr.is_null() && err_occurred() {
        return Err(PyErr);
    }
    let a = tuple::tuple_to_vec(args[1]);
    funcptr::callproc(addr, &a, flags, None, None, None)
}

fn call_function(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    call_function_impl(args, 0)
}

fn call_cdeclfunction(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    call_function_impl(args, FUNCFLAG_CDECL)
}

fn obj_from_ptr(_m: &PyObject, a: &PyObject) -> PyResult {
    let p = unsafe { capi::PyLong_AsVoidPtr(a.as_ptr()) } as *mut PyObject;
    if p.is_null() {
        if err_occurred() {
            return Err(PyErr);
        }
        raise!(ValueError, "NULL pointer access");
    }
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.PyObj_FromPtr"), cs!("(O)"), p) < 0 {
            return Err(PyErr);
        }
        Ok(PyRef::from_borrowed(p))
    }
}

fn py_incref(_m: &PyObject, a: &PyObject) -> PyResult {
    unsafe { incref(a.as_ptr()) };
    Ok(a.to_ref())
}

fn py_decref(_m: &PyObject, a: &PyObject) -> PyResult {
    unsafe { decref(a.as_ptr()) };
    Ok(a.to_ref())
}

fn unpickle(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 || !is_tuple(args[1]) {
        raise!(TypeError, "_unpickle expects (type, tuple)");
    }
    let tp = args[0];
    let new = getattr(tp, "__new__")?;
    let obj = call(&new, &[tp])?;
    let st = getattr(&obj, "__setstate__")?;
    let items = tuple::tuple_items(args[1]);
    call(&st, items)?;
    Ok(obj)
}

fn module_getattr(_m: &PyObject, name: &PyObject) -> PyResult {
    if is_str(name) && str_of(name) == "__version__" {
        unsafe {
            if capi::PyErr_WarnEx(capi::exc::DeprecationWarning(), cs!("'__version__' is deprecated and slated for removal in Python 3.20"), 1) < 0 {
                return Err(PyErr);
            }
        }
        return Ok(new_str("1.1.0"));
    }
    raise!(AttributeError, "module '_ctypes' has no attribute '{}'", str_of(name))
}

// ---- C helpers used by ctypes/__init__.py through function pointers

unsafe extern "C" fn c_string_at(ptr: *const u8, size: isize) -> *mut PyObject {
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.string_at"), cs!("nn"), ptr as isize, size) < 0 {
            return std::ptr::null_mut();
        }
        if size == -1 {
            return capi::PyBytes_FromString(ptr as *const std::ffi::c_char);
        }
        capi::PyBytes_FromStringAndSize(ptr as *const std::ffi::c_char, size)
    }
}

unsafe extern "C" fn c_wstring_at(ptr: *const i32, size: isize) -> *mut PyObject {
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.wstring_at"), cs!("nn"), ptr as isize, size) < 0 {
            return std::ptr::null_mut();
        }
        let mut n = size;
        if n == -1 {
            n = 0;
            while *ptr.offset(n) != 0 {
                n += 1;
            }
        }
        capi::PyUnicode_FromWideChar(ptr, n)
    }
}

unsafe extern "C" fn c_memoryview_at(ptr: *mut u8, size: isize, readonly: c_int) -> *mut PyObject {
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.memoryview_at"), cs!("nni"), ptr as isize, size, readonly) < 0 {
            return std::ptr::null_mut();
        }
        if size < 0 {
            let _ = err!(ValueError, "negative size");
            return std::ptr::null_mut();
        }
        capi::PyMemoryView_FromMemory(ptr as *mut std::ffi::c_char, size, if readonly != 0 { 0x100 } else { 0x200 })
    }
}

unsafe extern "C" fn c_cast(ptr: *mut c_void, src: *mut PyObject, ctype: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(cast_impl(ptr, &*src, &*ctype)) }
}

fn cast_check_pointertype(ctype: &PyObject) -> PyResult<()> {
    if let Some(i) = stginfo_of_type(ctype) {
        match i.kind {
            Kind::Pointer | Kind::FuncPtr => return Ok(()),
            Kind::Simple if matches!(i.code, b's' | b'z' | b'Z' | b'P') => return Ok(()),
            _ => {}
        }
    }
    raise!(TypeError, "cast() argument 2 must be a pointer type, not {}", if is_type(ctype) { type_name(ctype.as_ptr() as *mut PyTypeObject) } else { tname(ctype) })
}

fn cast_impl(ptr: *mut c_void, src: &PyObject, ctype: &PyObject) -> PyResult {
    cast_check_pointertype(ctype)?;
    let result = call(ctype, &[])?;
    // keep the source's objects alive in the result
    if is_cdata(src) {
        let rc = cd(&result);
        // result._objects = {id(src): src} merged with src's objects
        let container = get_container(&result);
        let cc = cdp(container);
        let _ = rc;
        let srcobjs = get_kept_objects(src);
        unsafe {
            if cc.b_objects.is_null() || !is_dict_exact(&*cc.b_objects) {
                let old = cc.b_objects;
                cc.b_objects = rpy_runtime::types::dict::new_dict().into_raw();
                xdecref(old);
            }
            let d = &*cc.b_objects;
            if !is_none(&srcobjs) {
                if is_dict_exact(&srcobjs) {
                    if capi::PyDict_Update(d.as_ptr(), srcobjs.as_ptr()) < 0 {
                        return Err(PyErr);
                    }
                }
            }
            let key = new_int(src.as_ptr() as usize as i64);
            rpy_runtime::types::dict::set_item(d, &key, src)?;
        }
    }
    unsafe { *(cd(&result).b_ptr as *mut *mut c_void) = ptr };
    Ok(result)
}

pub fn init() -> PyResult {
    super::init_ctype_type()?;
    init_cdata_type()?;
    super::cfield::init_type()?;
    super::simple::init_types()?;
    super::structs::init_types()?;
    super::array::init_types()?;
    super::pointer::init_types()?;
    super::funcptr::init_types()?;

    let methods = rpy_runtime::mdefs![
        ("sizeof", O, sizeof, "sizeof(C type) -> integer\nsizeof(C instance) -> integer\nReturn the size in bytes of a C instance"),
        ("alignment", O, alignment, "alignment(C type) -> integer\nalignment(C instance) -> integer\nReturn the alignment requirements of a C instance"),
        ("addressof", O, addressof, "addressof(C instance) -> integer\nReturn the address of the C instance internal buffer"),
        ("byref", FAST, byref, "byref(C instance[, offset=0]) -> byref-object\nReturn a pointer lookalike to a C instance, only usable\nas function argument"),
        ("resize", FAST, resize, "Resize the memory buffer of a ctypes instance"),
        ("buffer_info", O, buffer_info, "Return buffer interface information"),
        ("get_errno", NOARGS, get_errno),
        ("set_errno", O, set_errno),
        ("dlopen", FAST, py_dlopen, "dlopen(name, flag={RTLD_GLOBAL|RTLD_LOCAL}) open a shared library"),
        ("dlclose", O, py_dlclose, "dlclose a library"),
        ("dlsym", FAST, py_dlsym, "find symbol in shared library"),
        ("call_function", FAST, call_function),
        ("call_cdeclfunction", FAST, call_cdeclfunction),
        ("PyObj_FromPtr", O, obj_from_ptr),
        ("Py_INCREF", O, py_incref),
        ("Py_DECREF", O, py_decref),
        ("_unpickle", FAST, unpickle),
        ("__getattr__", O, module_getattr),
    ];
    let (m, d) = rpy_runtime::modules::fncommon::new_module("_ctypes", methods, Some("Create and manipulate C compatible data types in Python."))?;
    unsafe {
        MODULE = m.clone().into_raw();
    }
    let add = |n: &str, v: &PyObject| rpy_runtime::types::dict::set_item_str(&d, n, v);
    let ty = |t: *mut PyTypeObject| rpy_runtime::types::typeobject::type_obj(t);
    add("_SimpleCData", ty(simple::simple_type()))?;
    add("Structure", ty(structs::struct_type()))?;
    add("Union", ty(structs::union_type()))?;
    add("Array", ty(array::array_type()))?;
    add("_Pointer", ty(pointer::pointer_type()))?;
    add("CFuncPtr", ty(funcptr::funcptr_type()))?;
    add("CField", ty(cfield::cfield_type()))?;
    // ArgumentError
    let ae = from_c(unsafe { capi::PyErr_NewException(cs!("ctypes.ArgumentError"), std::ptr::null_mut(), std::ptr::null_mut()) })?;
    unsafe {
        ARGUMENT_ERROR = ae.clone().into_raw();
    }
    add("ArgumentError", &ae)?;
    for (n, v) in [
        ("FUNCFLAG_CDECL", FUNCFLAG_CDECL as i64),
        ("FUNCFLAG_USE_ERRNO", FUNCFLAG_USE_ERRNO as i64),
        ("FUNCFLAG_USE_LASTERROR", FUNCFLAG_USE_LASTERROR as i64),
        ("FUNCFLAG_PYTHONAPI", FUNCFLAG_PYTHONAPI as i64),
        ("RTLD_LOCAL", libc::RTLD_LOCAL as i64),
        ("RTLD_GLOBAL", libc::RTLD_GLOBAL as i64),
        ("CTYPES_MAX_ARGCOUNT", 1024),
        ("SIZEOF_TIME_T", std::mem::size_of::<libc::time_t>() as i64),
        ("_string_at_addr", c_string_at as *const () as usize as i64),
        ("_wstring_at_addr", c_wstring_at as *const () as usize as i64),
        ("_memoryview_at_addr", c_memoryview_at as *const () as usize as i64),
        ("_cast_addr", c_cast as *const () as usize as i64),
        ("_memmove_addr", libc::memmove as *const () as usize as i64),
        ("_memset_addr", libc::memset as *const () as usize as i64),
    ] {
        add(n, &new_int(v))?;
    }
    add("_ffi_available", &rpy_runtime::types::bool_::new_bool(libffi::lib().is_ok()))?;
    Ok(m)
}
