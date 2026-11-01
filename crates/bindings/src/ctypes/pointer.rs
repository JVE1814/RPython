//! Pointers: the `PyCPointerType` metaclass and `_Pointer` instances.

use super::cdata::*;
use super::*;
use rpy_runtime::types::{bytes, list};

pub static mut POINTER_META: PyTypeObject = PyTypeObject::ZERO;
pub static mut POINTER_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn pointer_meta() -> *mut PyTypeObject {
    &raw mut POINTER_META
}
pub fn pointer_type() -> *mut PyTypeObject {
    &raw mut POINTER_TYPE
}

fn own_type_attr(tp: *mut PyTypeObject) -> Option<PyRef> {
    unsafe {
        let d = (*tp).tp_dict;
        if d.is_null() {
            return None;
        }
        rpy_runtime::types::dict::get_item_str(&*d, &new_str("_type_"))
    }
}

fn set_proto(info: &mut StgInfo, proto: &PyObject) -> PyResult<()> {
    if !is_type(proto) {
        raise!(TypeError, "_type_ must be a type");
    }
    let pinfo = match stginfo_of_type(proto) {
        Some(i) => i,
        None => raise!(TypeError, "_type_ must have storage info"),
    };
    info.proto = Some(proto.to_ref());
    info.format = Some(format!("&{}", pinfo.format.clone().unwrap_or_else(|| "B".to_string())));
    Ok(())
}

unsafe extern "C" fn pointer_meta_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let tp_init = (*rpy_runtime::types::typeobject::type_type()).tp_init.unwrap();
        if tp_init(o, args, kw) < 0 {
            return -1;
        }
        int_into_c(pointer_meta_init_impl(&*o))
    }
}

fn new_pointer_info() -> StgInfo {
    let mut info = StgInfo::new(Kind::Pointer);
    info.size = 8;
    info.align = 8;
    info.length = 1;
    *info.ffi = unsafe { *libffi::types().pointer };
    info.dict_flags |= TYPEFLAG_ISPOINTER;
    info.format = Some("&B".to_string());
    info
}

fn pointer_meta_init_impl(cls: &PyObject) -> PyResult<()> {
    let tp = cls.as_ptr() as *mut PyTypeObject;
    if has_stginfo_slot_set(tp) {
        raise!(SystemError, "StgInfo of '{}' is already initialized.", type_name(tp));
    }
    let mut info = new_pointer_info();
    let proto = own_type_attr(tp);
    if let Some(p) = &proto {
        set_proto(&mut info, p)?;
    }
    init_stginfo(tp, info)?;
    if let Some(p) = proto {
        if let Some(pi) = stginfo_of_type(&p) {
            pi.pointer_type = Some(cls.to_ref());
        }
    }
    Ok(())
}

fn pointer_set_type(cls: &PyObject, typ: &PyObject) -> PyResult {
    let info = match stginfo_of_type(cls) {
        Some(i) => i,
        None => raise!(TypeError, "abstract class"),
    };
    set_proto(info, typ)?;
    if let Some(pi) = stginfo_of_type(typ) {
        if pi.pointer_type.is_none() {
            pi.pointer_type = Some(cls.to_ref());
        }
    }
    setattr(cls, "_type_", typ)?;
    Ok(none())
}

fn pointer_from_param(cls: &PyObject, value: &PyObject) -> PyResult {
    if is_none(value) {
        return Ok(none());
    }
    let info = match stginfo_of_type(cls) {
        Some(i) => i,
        None => raise!(TypeError, "abstract class"),
    };
    if let Some(proto) = &info.proto {
        if isinstance(value, proto)? {
            // `POINTER(c_int).from_param(c_int(5))` passes a pointer to it
            return funcptr::byref_impl(value, 0);
        }
        if is_cdata(value) {
            if let Some(vi) = stginfo_of_obj(value) {
                if vi.kind == Kind::Pointer || vi.kind == Kind::Array {
                    if let Some(vp) = &vi.proto {
                        if rpy_runtime::abstract_::issubclass(vp, proto)? {
                            return Ok(value.to_ref());
                        }
                    }
                }
            }
        }
    }
    ct_from_param(cls, value)
}

// ------------------------------------------------------------------ instances

fn proto_of(o: &PyObject) -> PyResult<(PyRef, isize)> {
    let info = match stginfo_of_obj(o) {
        Some(i) => i,
        None => raise!(TypeError, "abstract class"),
    };
    match &info.proto {
        Some(p) => {
            let sz = stginfo_of_type(p).map_or(0, |i| i.size);
            Ok((p.clone(), sz))
        }
        None => raise!(TypeError, "Cannot create instance: has no _type_"),
    }
}

fn deref(o: &PyObject) -> *mut u8 {
    unsafe { *(cd(o).b_ptr as *const *mut u8) }
}

fn contents_get(o: &PyObject) -> PyResult {
    let p = deref(o);
    if p.is_null() {
        raise!(ValueError, "NULL pointer access");
    }
    let (proto, _) = proto_of(o)?;
    from_base_obj(proto.as_ptr() as *mut PyTypeObject, Some(o), 0, p)
}

fn contents_set(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(TypeError, "Pointer does not support item deletion"),
    };
    let (proto, _) = proto_of(o)?;
    if !isinstance(v, &proto)? {
        raise!(TypeError, "expected {} instead of {}", type_name(proto.as_ptr() as *mut PyTypeObject), tname(v));
    }
    unsafe { *(cd(o).b_ptr as *mut *mut u8) = cd(v).b_ptr };
    // keep the pointed-to object and its own keep-alive objects
    keep_ref(o, 1, Some(v.to_ref()))?;
    let kept = get_kept_objects(v);
    keep_ref(o, 0, Some(kept))
}

fn item_get(o: &PyObject, index: isize) -> PyResult {
    let p = deref(o);
    if p.is_null() {
        raise!(ValueError, "NULL pointer access");
    }
    let (proto, size) = proto_of(o)?;
    let offset = index.wrapping_mul(size);
    cdata_get(&proto, FieldCode::None, Some(o), index, size, None, unsafe { p.offset(offset) })
}

fn item_set(o: &PyObject, index: isize, v: &PyObject) -> PyResult<()> {
    let p = deref(o);
    if p.is_null() {
        raise!(ValueError, "NULL pointer access");
    }
    let (proto, size) = proto_of(o)?;
    let offset = index.wrapping_mul(size);
    cdata_set(o, &proto, FieldCode::None, v, index, size, None, unsafe { p.offset(offset) })
}

unsafe extern "C" fn pointer_item(o: *mut PyObject, i: isize) -> *mut PyObject {
    into_c(item_get(r(o), i))
}

unsafe extern "C" fn pointer_ass_item(o: *mut PyObject, i: isize, v: *mut PyObject) -> c_int {
    if v.is_null() {
        let _ = err!(TypeError, "Pointer does not support item deletion");
        return -1;
    }
    int_into_c(item_set(r(o), i, r(v)))
}

fn is_slice(o: &PyObject) -> bool {
    unsafe { o.type_ptr() == &raw mut rpy_runtime::types::slice::PySlice_Type }
}

fn subscript(o: &PyObject, key: &PyObject) -> PyResult {
    if rpy_runtime::abstract_::has_index(key) {
        let i = ssize(key)?;
        return item_get(o, i);
    }
    if !is_slice(key) {
        raise!(TypeError, "Pointer indices must be integer");
    }
    let start_o = getattr(key, "start")?;
    let stop_o = getattr(key, "stop")?;
    let step_o = getattr(key, "step")?;
    let step = if is_none(&step_o) { 1 } else { ssize(&step_o)? };
    if step == 0 {
        raise!(ValueError, "slice step cannot be zero");
    }
    let start = if is_none(&start_o) {
        if step < 0 {
            raise!(ValueError, "slice start is required for step < 0");
        }
        0
    } else {
        ssize(&start_o)?
    };
    if is_none(&stop_o) {
        raise!(ValueError, "slice stop is required");
    }
    let stop = ssize(&stop_o)?;
    let len = if (step > 0 && start > stop) || (step < 0 && start < stop) {
        0
    } else if step > 0 {
        (stop - start - 1) / step + 1
    } else {
        (stop - start + 1) / step + 1
    };
    let (proto, _) = proto_of(o)?;
    if let Some(pi) = stginfo_of_type(&proto) {
        if pi.kind == Kind::Simple && !simple_instance(proto.as_ptr() as *mut PyTypeObject) {
            let p = deref(o);
            if pi.code == b'c' {
                let mut out = Vec::with_capacity(len.max(0) as usize);
                let mut cur = start;
                for _ in 0..len {
                    out.push(unsafe { *p.offset(cur) });
                    cur += step;
                }
                return Ok(bytes::new_bytes(&out));
            }
            if pi.code == b'u' {
                let mut out: Vec<i32> = Vec::with_capacity(len.max(0) as usize);
                let mut cur = start;
                for _ in 0..len {
                    out.push(unsafe { *(p as *const i32).offset(cur) });
                    cur += step;
                }
                return simple::wide_to_str(out.as_ptr(), out.len());
            }
        }
    }
    let l = list::new_list(Vec::new());
    let mut cur = start;
    for _ in 0..len {
        list::append(&l, item_get(o, cur)?);
        cur += step;
    }
    Ok(l)
}

unsafe extern "C" fn pointer_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    into_c(subscript(r(o), r(k)))
}

unsafe extern "C" fn pointer_ass_subscript(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    let r_ = (|| -> PyResult<()> {
        if v.is_null() {
            raise!(TypeError, "Pointer does not support item deletion");
        }
        let i = ssize(r(k))?;
        item_set(r(o), i, r(v))
    })();
    int_into_c(r_)
}

unsafe extern "C" fn pointer_bool(o: *mut PyObject) -> c_int {
    if deref(r(o)).is_null() { 0 } else { 1 }
}

unsafe extern "C" fn pointer_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    let r_ = (|| -> PyResult {
        match stginfo(tp) {
            Some(i) if i.proto.is_some() => {}
            Some(_) => raise!(TypeError, "Cannot create instance: has no _type_"),
            None => raise!(TypeError, "abstract class"),
        }
        generic_new(tp)
    })();
    into_c(r_)
}

unsafe extern "C" fn pointer_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let a = rpy_runtime::modules::fncommon::targs(args);
        let r_ = (|| -> PyResult<()> {
            if rpy_runtime::modules::fncommon::kw_len(kw) > 0 {
                raise!(TypeError, "_Pointer() takes no keyword arguments");
            }
            if a.len() > 1 {
                raise!(TypeError, "POINTER() takes at most 1 argument ({} given)", a.len());
            }
            if a.len() == 1 {
                contents_set(&*o, Some(a[0]))?;
            }
            Ok(())
        })();
        int_into_c(r_)
    }
}

fn pointer_class_getitem(cls: &PyObject, item: &PyObject) -> PyResult {
    rpy_runtime::modules::fncommon::class_getitem(cls, item)
}

static mut POINTER_AS_SEQUENCE: PySequenceMethods = unsafe { std::mem::zeroed() };
static mut POINTER_AS_MAPPING: PyMappingMethods = unsafe { std::mem::zeroed() };
static mut POINTER_AS_NUMBER: PyNumberMethods = unsafe { std::mem::zeroed() };

pub fn init_types() -> PyResult<()> {
    unsafe {
        let m = pointer_meta();
        init_meta(m, c"_ctypes.PyCPointerType", ctype_type());
        (*m).tp_init = Some(pointer_meta_init);
        let extra = {
            let t = rpy_runtime::mdefs![("set_type", O, pointer_set_type)];
            vec![std::ptr::read(t)]
        };
        (*m).tp_methods = meta_methods(rpy_runtime::mdefs![("from_param", O, pointer_from_param, "Convert a Python object into a function call parameter.")], extra);
        (*m).tp_doc = c"metatype for the Pointer Objects".as_ptr();
        set_meta_number(m);
        rpy_runtime::types::typeobject::type_ready(m)?;

        let t = pointer_type();
        init_base_type(t, c"_ctypes._Pointer", m, std::mem::size_of::<CDataObject>());
        (*t).tp_new = Some(pointer_new);
        (*t).tp_init = Some(pointer_init);
        POINTER_AS_SEQUENCE.sq_item = Some(pointer_item);
        POINTER_AS_SEQUENCE.sq_ass_item = Some(pointer_ass_item);
        POINTER_AS_MAPPING.mp_subscript = Some(pointer_subscript);
        POINTER_AS_MAPPING.mp_ass_subscript = Some(pointer_ass_subscript);
        POINTER_AS_NUMBER.nb_bool = Some(pointer_bool);
        (*t).tp_as_sequence = &raw mut POINTER_AS_SEQUENCE;
        (*t).tp_as_mapping = &raw mut POINTER_AS_MAPPING;
        (*t).tp_as_number = &raw mut POINTER_AS_NUMBER;
        (*t).tp_doc = c"XXX to be provided".as_ptr();
        (*t).tp_getset = rpy_runtime::getset_table![("contents", contents_get, contents_set; "the object this pointer points to (read-write)")];
        (*t).tp_methods = rpy_runtime::mdefs![("__class_getitem__", O, pointer_class_getitem; METH_CLASS)];
        rpy_runtime::types::typeobject::type_ready(t)?;
    }
    Ok(())
}
