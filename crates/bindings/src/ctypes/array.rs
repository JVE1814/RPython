//! Arrays: the `PyCArrayType` metaclass, `Array` instances (sequence protocol,
//! slicing, `value`/`raw` of character arrays) and `type * n` creation.

use super::cdata::*;
use super::libffi::FfiType;
use super::*;
use rpy_runtime::types::{bytes, dict, list, tuple};

pub static mut ARRAY_META: PyTypeObject = PyTypeObject::ZERO;
pub static mut ARRAY_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn array_meta() -> *mut PyTypeObject {
    &raw mut ARRAY_META
}
pub fn array_type() -> *mut PyTypeObject {
    &raw mut ARRAY_TYPE
}

/// libffi type of the innermost element and the total element count.
pub fn flat_item_ffi(info: &StgInfo) -> (*mut FfiType, usize) {
    let mut n = info.length.max(0) as usize;
    let mut cur = info.proto.as_ref().and_then(|p| stginfo_of_type(p));
    while let Some(i) = cur {
        match i.kind {
            Kind::Array => {
                n *= i.length.max(0) as usize;
                cur = i.proto.as_ref().and_then(|p| stginfo_of_type(p));
            }
            Kind::Simple => return (simple::code_ffi(i.code), n),
            Kind::Struct | Kind::Union => return (i.ffi_ptr(), n),
            _ => return (libffi::types().pointer, n),
        }
    }
    (libffi::types().uint8, n)
}

// ------------------------------------------------------------------ metaclass

unsafe extern "C" fn array_meta_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let tp_init = (*rpy_runtime::types::typeobject::type_type()).tp_init.unwrap();
        if tp_init(o, args, kw) < 0 {
            return -1;
        }
        int_into_c(array_meta_init_impl(&*o))
    }
}

fn array_meta_init_impl(cls: &PyObject) -> PyResult<()> {
    let tp = cls.as_ptr() as *mut PyTypeObject;
    if has_stginfo_slot_set(tp) {
        raise!(SystemError, "StgInfo of '{}' is already initialized.", type_name(tp));
    }
    let length_attr = match getattr_opt(cls, "_length_")? {
        Some(l) => l,
        None => raise!(AttributeError, "class must define a '_length_' attribute"),
    };
    if !is_int(&length_attr) {
        raise!(TypeError, "The '_length_' attribute must be an integer");
    }
    let length = match rpy_runtime::types::int::as_i64(&length_attr) {
        Some(v) if v >= 0 && v <= isize::MAX as i64 => v as isize,
        Some(v) if v < 0 => raise!(ValueError, "The '_length_' attribute must not be negative"),
        _ => {
            if rpy_runtime::types::int::sign(&length_attr) < 0 {
                raise!(ValueError, "The '_length_' attribute must not be negative");
            }
            raise!(OverflowError, "The '_length_' attribute is too large");
        }
    };
    let proto = match getattr_opt(cls, "_type_")? {
        Some(p) => p,
        None => raise!(AttributeError, "class must define a '_type_' attribute"),
    };
    let pinfo = match stginfo_of_type(&proto) {
        Some(i) => i,
        None => raise!(TypeError, "_type_ must have storage info"),
    };
    let itemsize = pinfo.size;
    if itemsize != 0 && length > isize::MAX / itemsize {
        raise!(OverflowError, "array too large");
    }
    let mut info = StgInfo::new(Kind::Array);
    info.size = itemsize * length;
    info.align = pinfo.align;
    info.length = length;
    info.proto = Some(proto.clone());
    info.dict_flags = pinfo.dict_flags & (TYPEFLAG_HASPOINTER | TYPEFLAG_HASUNION | TYPEFLAG_HASBITFIELD);
    if pinfo.dict_flags & TYPEFLAG_ISPOINTER != 0 || pinfo.kind == Kind::Pointer {
        info.dict_flags |= TYPEFLAG_HASPOINTER;
    }
    // libffi: arrays are passed as pointers
    *info.ffi = unsafe { *libffi::types().pointer };
    // PEP 3118: item format with the accumulated shape
    let mut shape = vec![length];
    shape.extend(pinfo.shape.iter().copied());
    info.shape = shape;
    info.format = Some(pinfo.format.clone().unwrap_or_else(|| "B".to_string()));
    let is_char = pinfo.kind == Kind::Simple && pinfo.code == b'c';
    let is_wchar = pinfo.kind == Kind::Simple && pinfo.code == b'u';
    init_stginfo(tp, info)?;
    // character arrays get value/raw
    unsafe {
        let d = &*(*tp).tp_dict;
        if is_char {
            let gs = rpy_runtime::getset_table![
                ("raw", char_array_get_raw, char_array_set_raw; "value"),
                ("value", char_array_get_value, char_array_set_value; "string value"),
            ];
            add_getsets(tp, d, gs)?;
        } else if is_wchar {
            let gs = rpy_runtime::getset_table![("value", wchar_array_get_value, wchar_array_set_value; "string value")];
            add_getsets(tp, d, gs)?;
        }
        capi::PyType_Modified(tp);
    }
    Ok(())
}

unsafe fn add_getsets(tp: *mut PyTypeObject, d: &PyObject, mut gs: *mut PyGetSetDef) -> PyResult<()> {
    unsafe {
        while !(*gs).name.is_null() {
            let descr = from_c(capi::PyDescr_NewGetSet(tp, gs))?;
            let nm = std::ffi::CStr::from_ptr((*gs).name).to_string_lossy().into_owned();
            dict::set_item(d, &new_str(&nm), &descr)?;
            gs = gs.add(1);
        }
    }
    Ok(())
}

fn char_array_get_raw(o: &PyObject) -> PyResult {
    let c = cd(o);
    Ok(bytes::new_bytes(unsafe { std::slice::from_raw_parts(c.b_ptr, c.b_size.max(0) as usize) }))
}

fn char_array_set_raw(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(AttributeError, "cannot delete attribute"),
    };
    let mut view: Py_buffer = unsafe { std::mem::zeroed() };
    unsafe {
        if capi::PyObject_GetBuffer(v.as_ptr(), &mut view, 0) < 0 {
            return Err(PyErr);
        }
    }
    let c = cd(o);
    let n = view.len;
    let r = if n > c.b_size {
        Err(err!(ValueError, "byte string too long"))
    } else {
        unsafe { std::ptr::copy(view.buf as *const u8, c.b_ptr, n as usize) };
        Ok(())
    };
    unsafe { capi::PyBuffer_Release(&mut view) };
    r
}

fn char_array_get_value(o: &PyObject) -> PyResult {
    let c = cd(o);
    let s = unsafe { std::slice::from_raw_parts(c.b_ptr, c.b_size.max(0) as usize) };
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    Ok(bytes::new_bytes(&s[..end]))
}

fn char_array_set_value(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(TypeError, "can't delete attribute"),
    };
    if !is_bytes(v) {
        raise!(TypeError, "bytes expected instead of {} instance", tname(v));
    }
    let data = bytes::bytes_data(v);
    let c = cd(o);
    let size = c.b_size.max(0) as usize;
    if data.len() > size {
        raise!(ValueError, "byte string too long");
    }
    unsafe {
        std::ptr::copy(data.as_ptr(), c.b_ptr, data.len());
        if data.len() < size {
            *c.b_ptr.add(data.len()) = 0;
        }
    }
    Ok(())
}

fn wchar_array_get_value(o: &PyObject) -> PyResult {
    let c = cd(o);
    let n = (c.b_size.max(0) as usize) / 4;
    let s = unsafe { std::slice::from_raw_parts(c.b_ptr as *const i32, n) };
    let end = s.iter().position(|&b| b == 0).unwrap_or(n);
    simple::wide_to_str(s.as_ptr(), end)
}

fn wchar_array_set_value(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(TypeError, "can't delete attribute"),
    };
    if !is_str(v) {
        raise!(TypeError, "unicode string expected instead of {} instance", tname(v));
    }
    let w = simple::str_to_wide_bytes(v)?;
    let wd = bytes::bytes_data(&w);
    let nchars = wd.len() / 4 - 1;
    let c = cd(o);
    let cap = (c.b_size.max(0) as usize) / 4;
    if nchars > cap {
        raise!(ValueError, "string too long");
    }
    let ncopy = if nchars < cap { nchars + 1 } else { nchars };
    unsafe { std::ptr::copy(wd.as_ptr(), c.b_ptr, ncopy * 4) };
    Ok(())
}

// ------------------------------------------------------------------ type creation cache

static ARRAY_CACHE: std::sync::Mutex<Vec<(usize, isize, usize)>> = std::sync::Mutex::new(Vec::new());

/// `itemtype * length` (cached, like CPython).
pub fn array_type_from_ctype(itemtype: &PyObject, length: isize) -> PyResult {
    // cache entries hold weak references to the item type and strong ones to the array type
    {
        let g = ARRAY_CACHE.lock().unwrap();
        for &(it, len, arr) in g.iter() {
            if it == itemtype.as_ptr() as usize && len == length {
                return Ok(unsafe { PyRef::from_borrowed(arr as *mut PyObject) });
            }
        }
    }
    if !is_type(itemtype) {
        raise!(TypeError, "Expected a type object");
    }
    let name = format!("{}_Array_{}", type_name(itemtype.as_ptr() as *mut PyTypeObject), length);
    let d = dict::new_dict();
    dict::set_item(&d, &new_str("_length_"), &new_int(length as i64))?;
    dict::set_item(&d, &new_str("_type_"), itemtype)?;
    let bases = tuple::new_tuple(vec![rpy_runtime::types::typeobject::type_ref(array_type())]);
    let meta = rpy_runtime::types::typeobject::type_obj(array_meta());
    let r = call(meta, &[&new_str(&name), &bases, &d])?;
    let mut g = ARRAY_CACHE.lock().unwrap();
    // the cache keeps both alive (like CPython's cache dict keyed by (itemtype, length))
    std::mem::forget(itemtype.to_ref());
    g.push((itemtype.as_ptr() as usize, length, r.clone().into_raw() as usize));
    Ok(r)
}

// ------------------------------------------------------------------ instances

fn item_info(o: &PyObject) -> PyResult<(&'static mut StgInfo, PyRef, isize)> {
    let info = match stginfo_of_obj(o) {
        Some(i) => i,
        None => raise!(TypeError, "abstract class"),
    };
    let proto = match &info.proto {
        Some(p) => p.clone(),
        None => raise!(TypeError, "abstract class"),
    };
    let isz = stginfo_of_type(&proto).map_or(0, |i| i.size);
    Ok((info, proto, isz))
}

fn array_item_impl(o: &PyObject, index: isize) -> PyResult {
    let c = cd(o);
    if index < 0 || index >= c.b_length {
        raise!(IndexError, "invalid index");
    }
    let (_, proto, isz) = item_info(o)?;
    let ptr = unsafe { c.b_ptr.offset(index * isz) };
    cdata_get(&proto, FieldCode::None, Some(o), index, isz, None, ptr)
}

fn array_setitem_impl(o: &PyObject, index: isize, v: &PyObject) -> PyResult<()> {
    let c = cd(o);
    if index < 0 || index >= c.b_length {
        raise!(IndexError, "invalid index");
    }
    let (_, proto, isz) = item_info(o)?;
    let ptr = unsafe { c.b_ptr.offset(index * isz) };
    cdata_set(o, &proto, FieldCode::None, v, index, isz, None, ptr)
}

unsafe extern "C" fn array_length(o: *mut PyObject) -> isize {
    cdp(o).b_length
}

unsafe extern "C" fn array_item(o: *mut PyObject, i: isize) -> *mut PyObject {
    into_c(array_item_impl(r(o), i))
}

unsafe extern "C" fn array_ass_item(o: *mut PyObject, i: isize, v: *mut PyObject) -> c_int {
    if v.is_null() {
        let _ = err!(TypeError, "Array does not support item deletion");
        return -1;
    }
    int_into_c(array_setitem_impl(r(o), i, r(v)))
}

fn slice_indices(key: &PyObject, len: isize) -> PyResult<(isize, isize, isize, isize)> {
    let (mut start, mut stop, mut step) = (0isize, 0isize, 0isize);
    unsafe {
        if capi::PySlice_Unpack(key.as_ptr(), &mut start, &mut stop, &mut step) < 0 {
            return Err(PyErr);
        }
        let n = capi::PySlice_AdjustIndices(len, &mut start, &mut stop, step);
        Ok((start, stop, step, n))
    }
}

fn is_slice(o: &PyObject) -> bool {
    unsafe { o.type_ptr() == &raw mut rpy_runtime::types::slice::PySlice_Type }
}

pub fn subscript_impl(o: &PyObject, key: &PyObject) -> PyResult {
    if rpy_runtime::abstract_::has_index(key) {
        let mut i = ssize(key)?;
        if i < 0 {
            i += cd(o).b_length;
        }
        return array_item_impl(o, i);
    }
    if is_slice(key) {
        let (start, _stop, step, n) = slice_indices(key, cd(o).b_length)?;
        let (_, proto, isz) = item_info(o)?;
        if let Some(pi) = stginfo_of_type(&proto) {
            if pi.kind == Kind::Simple && !simple_instance(proto.as_ptr() as *mut PyTypeObject) {
                let c = cd(o);
                if pi.code == b'c' {
                    let mut out = Vec::with_capacity(n.max(0) as usize);
                    let mut cur = start;
                    for _ in 0..n {
                        out.push(unsafe { *c.b_ptr.offset(cur) });
                        cur = cur.wrapping_add(step);
                    }
                    return Ok(bytes::new_bytes(&out));
                }
                if pi.code == b'u' {
                    let mut out: Vec<i32> = Vec::with_capacity(n.max(0) as usize);
                    let mut cur = start;
                    for _ in 0..n {
                        out.push(unsafe { *(c.b_ptr as *const i32).offset(cur) });
                        cur = cur.wrapping_add(step);
                    }
                    return simple::wide_to_str(out.as_ptr(), out.len());
                }
            }
        }
        let _ = isz;
        let l = list::new_list(Vec::new());
        let mut cur = start;
        for _ in 0..n {
            let v = array_item_impl(o, cur)?;
            list::append(&l, v);
            cur = cur.wrapping_add(step);
        }
        return Ok(l);
    }
    raise!(TypeError, "indices must be integers")
}

pub fn ass_subscript_impl(o: &PyObject, key: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(TypeError, "Array does not support item deletion"),
    };
    if rpy_runtime::abstract_::has_index(key) {
        let mut i = ssize(key)?;
        if i < 0 {
            i += cd(o).b_length;
        }
        return array_setitem_impl(o, i, v);
    }
    if is_slice(key) {
        let (start, _stop, step, n) = slice_indices(key, cd(o).b_length)?;
        let other_len = rpy_runtime::abstract_::length(v)? as isize;
        if other_len != n {
            raise!(ValueError, "Can only assign sequence of same size");
        }
        let mut cur = start;
        for i in 0..n {
            let item = rpy_runtime::abstract_::getitem(v, &new_int(i as i64))?;
            array_setitem_impl(o, cur, &item)?;
            cur = cur.wrapping_add(step);
        }
        return Ok(());
    }
    raise!(TypeError, "indices must be integer")
}

unsafe extern "C" fn array_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    into_c(subscript_impl(r(o), r(k)))
}

unsafe extern "C" fn array_ass_subscript(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    int_into_c(ass_subscript_impl(r(o), r(k), if v.is_null() { None } else { Some(r(v)) }))
}

unsafe extern "C" fn array_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let a = rpy_runtime::modules::fncommon::targs(args);
        let r = (|| -> PyResult<()> {
            if rpy_runtime::modules::fncommon::kw_len(kw) > 0 {
                raise!(TypeError, "Array() takes no keyword arguments");
            }
            for (i, v) in a.iter().enumerate() {
                array_setitem_impl(&*o, i as isize, v)?;
            }
            Ok(())
        })();
        int_into_c(r)
    }
}

fn array_class_getitem(cls: &PyObject, item: &PyObject) -> PyResult {
    rpy_runtime::modules::fncommon::class_getitem(cls, item)
}

static mut ARRAY_AS_SEQUENCE: PySequenceMethods = unsafe { std::mem::zeroed() };
static mut ARRAY_AS_MAPPING: PyMappingMethods = unsafe { std::mem::zeroed() };

pub fn init_types() -> PyResult<()> {
    unsafe {
        let m = array_meta();
        init_meta(m, c"_ctypes.PyCArrayType", ctype_type());
        (*m).tp_init = Some(array_meta_init);
        (*m).tp_methods = meta_methods(rpy_runtime::mdefs![("from_param", O, ct_from_param, "Convert a Python object into a function call parameter.")], vec![]);
        (*m).tp_doc = c"metatype for the Array Objects".as_ptr();
        set_meta_number(m);
        rpy_runtime::types::typeobject::type_ready(m)?;

        let t = array_type();
        init_base_type(t, c"_ctypes.Array", m, std::mem::size_of::<CDataObject>());
        (*t).tp_init = Some(array_init);
        ARRAY_AS_SEQUENCE.sq_length = Some(array_length);
        ARRAY_AS_SEQUENCE.sq_item = Some(array_item);
        ARRAY_AS_SEQUENCE.sq_ass_item = Some(array_ass_item);
        ARRAY_AS_MAPPING.mp_length = Some(array_length);
        ARRAY_AS_MAPPING.mp_subscript = Some(array_subscript);
        ARRAY_AS_MAPPING.mp_ass_subscript = Some(array_ass_subscript);
        (*t).tp_as_sequence = &raw mut ARRAY_AS_SEQUENCE;
        (*t).tp_as_mapping = &raw mut ARRAY_AS_MAPPING;
        (*t).tp_flags |= Py_TPFLAGS_SEQUENCE;
        (*t).tp_doc = c"Abstract base class for arrays.".as_ptr();
        (*t).tp_methods = rpy_runtime::mdefs![("__class_getitem__", O, array_class_getitem; METH_CLASS)];
        rpy_runtime::types::typeobject::type_ready(t)?;
    }
    Ok(())
}
