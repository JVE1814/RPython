//! Structure and Union: metaclasses (`_fields_` handling, layout through
//! `ctypes._layout`), instance initialisation and anonymous fields.

use super::cdata::*;
use super::cfield::*;
use super::libffi::FfiType;
use super::*;
use rpy_runtime::types::{dict, tuple};

pub static mut STRUCT_META: PyTypeObject = PyTypeObject::ZERO;
pub static mut UNION_META: PyTypeObject = PyTypeObject::ZERO;
pub static mut STRUCT_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut UNION_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn struct_meta() -> *mut PyTypeObject {
    &raw mut STRUCT_META
}
pub fn union_meta() -> *mut PyTypeObject {
    &raw mut UNION_META
}
pub fn struct_type() -> *mut PyTypeObject {
    &raw mut STRUCT_TYPE
}
pub fn union_type() -> *mut PyTypeObject {
    &raw mut UNION_TYPE
}

fn own_dict_item(tp: *mut PyTypeObject, name: &str) -> Option<PyRef> {
    unsafe {
        let d = (*tp).tp_dict;
        if d.is_null() {
            return None;
        }
        dict::get_item_str(&*d, &new_str(name))
    }
}

fn is_struct_meta(tp: *mut PyTypeObject) -> bool {
    subtype(ob_type(tp as *mut PyObject), struct_meta())
}

// ------------------------------------------------------------------ metaclass

unsafe extern "C" fn struct_meta_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let tp_init = (*rpy_runtime::types::typeobject::type_type()).tp_init.unwrap();
        if tp_init(o, args, kw) < 0 {
            return -1;
        }
        let is_struct = is_struct_meta(o as *mut PyTypeObject);
        int_into_c(struct_meta_init_impl(&*o, is_struct))
    }
}

fn struct_meta_init_impl(cls: &PyObject, is_struct: bool) -> PyResult<()> {
    let tp = cls.as_ptr() as *mut PyTypeObject;
    if has_stginfo_slot_set(tp) {
        raise!(SystemError, "StgInfo of '{}' is already initialized.", type_name(tp));
    }
    if own_dict_item(tp, "_abstract_").is_some() {
        return Ok(());
    }
    let base = unsafe { (*tp).tp_base };
    let info = match stginfo(base) {
        Some(bi) => {
            bi.dict_flags |= DICTFLAG_FINAL;
            let mut n = bi.clone_for_subclass();
            n.kind = if is_struct { Kind::Struct } else { Kind::Union };
            n
        }
        None => {
            let mut n = StgInfo::new(if is_struct { Kind::Struct } else { Kind::Union });
            n.align = 1;
            n.ffi.type_ = libffi::FFI_TYPE_STRUCT;
            n.ffi.alignment = 1;
            n.ffi_elements = vec![std::ptr::null_mut()];
            n.ffi.elements = n.ffi_elements.as_mut_ptr();
            n.format = None;
            n
        }
    };
    init_stginfo(tp, info)?;
    if let Some(fields) = own_dict_item(tp, "_fields_") {
        setattr(cls, "_fields_", &fields)?;
    } else if let Some(anon) = own_dict_item(tp, "_anonymous_") {
        // anonymous names without fields still have to exist
        make_anon_fields(cls, &anon)?;
    }
    Ok(())
}

unsafe extern "C" fn struct_meta_setattro(o: *mut PyObject, name: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let ts = (*rpy_runtime::types::typeobject::type_type()).tp_setattro.unwrap();
        if !v.is_null() && is_str(&*name) && str_of(&*name) == "_fields_" {
            // validate and apply the layout before storing the attribute
            let is_struct = is_struct_meta(o as *mut PyTypeObject);
            if update_stginfo(&*o, &*v, is_struct).is_err() {
                return -1;
            }
        }
        ts(o, name, v)
    }
}

/// Apply a new `_fields_` to a structure/union type.
pub fn update_stginfo(cls: &PyObject, fields: &PyObject, is_struct: bool) -> PyResult<()> {
    let tp = cls.as_ptr() as *mut PyTypeObject;
    let info = match stginfo(tp) {
        Some(i) => i,
        None => raise!(TypeError, "ctypes state is not initialized"),
    };
    if info.dict_flags & DICTFLAG_FINAL != 0 {
        raise!(AttributeError, "_fields_ is final");
    }
    if let Ok(items) = rpy_runtime::abstract_::collect_iterable(fields) {
        for it in &items {
            if is_tuple(it) && tuple::tuple_len(it) >= 2 && tuple::tuple_get(it, 1).is(cls) {
                raise!(AttributeError, "Structure or union cannot contain itself");
            }
        }
    } else {
        rpy_runtime::errors::clear_exc();
    }
    let base = unsafe { (*tp).tp_base };
    let (base_size, base_align, base_length, base_obj) = match stginfo(base) {
        Some(bi) => (bi.size, bi.align, bi.length, rpy_runtime::types::typeobject::type_ref(base)),
        None => (0, 0, 0, none()),
    };
    let layout_mod = rpy_runtime::import::import_absolute("ctypes._layout")?;
    let get_layout = getattr(&layout_mod, "get_layout")?;
    let r = call(
        &get_layout,
        &[
            cls,
            fields,
            &rpy_runtime::types::bool_::new_bool(is_struct),
            &base_obj,
            &new_int(base_size as i64),
            &new_int(base_align as i64),
            &new_int(base_length as i64),
        ],
    )?;
    // (cfields, size, align, format)
    let fields_obj = getattr(&r, "fields")?;
    let cfields = rpy_runtime::abstract_::collect_iterable(&fields_obj)?;
    let size_obj = getattr(&r, "size")?;
    let size = ssize(&size_obj)?;
    let align_obj = getattr(&r, "align")?;
    let align = ssize(&align_obj)?;
    let fmt = getattr(&r, "format_spec")?;
    let fmt = if is_none(&fmt) { None } else { Some(str_of(&fmt)) };

    let mut flags = info.dict_flags & (TYPEFLAG_HASPOINTER | TYPEFLAG_HASUNION | TYPEFLAG_HASBITFIELD);
    if !is_struct {
        flags |= TYPEFLAG_HASUNION;
    }
    // ffi elements
    let t = libffi::types();
    let mut elements: Vec<*mut FfiType> = Vec::new();
    let mut children: Vec<Box<FfiType>> = Vec::new();
    if base_size > 0 {
        if let Some(bi) = stginfo(base) {
            let mut b = Box::new(*bi.ffi);
            // a copy of the base's elements list stays owned by the base type (kept alive
            // through the MRO), so pointing at it is fine
            b.elements = bi.ffi.elements;
            elements.push(&mut *b as *mut FfiType);
            children.push(b);
        }
    }
    let mut last_bit_storage: Option<isize> = None;
    let mut biggest: Option<(isize, *mut FfiType)> = None;
    for f in &cfields {
        if !is_cfield(f) {
            raise!(TypeError, "_layout.get_layout() must return CField objects");
        }
        let fd = field(f);
        let fi = match stginfo_of_type(&fd.proto) {
            Some(i) => i,
            None => raise!(TypeError, "field type must be a C type"),
        };
        if fi.dict_flags & (TYPEFLAG_ISPOINTER | TYPEFLAG_HASPOINTER) != 0 || fi.kind == Kind::Pointer {
            flags |= TYPEFLAG_HASPOINTER;
        }
        if fi.kind == Kind::Union || fi.dict_flags & TYPEFLAG_HASUNION != 0 {
            flags |= TYPEFLAG_HASUNION;
        }
        if fi.dict_flags & TYPEFLAG_HASBITFIELD != 0 {
            flags |= TYPEFLAG_HASBITFIELD;
        }
        // a type used as a field can no longer change its layout
        fi.dict_flags |= DICTFLAG_FINAL;
        let ft: *mut FfiType = match fi.kind {
            Kind::Simple => simple::code_ffi(fi.code),
            Kind::Struct | Kind::Union => fi.ffi_ptr(),
            Kind::Pointer | Kind::FuncPtr => t.pointer,
            Kind::Array => {
                // a struct of `n` items of the innermost element type
                let (item, n) = array::flat_item_ffi(fi);
                let mut ch = Box::new(FfiType { size: fi.size as usize, alignment: fi.align.max(1) as u16, type_: libffi::FFI_TYPE_STRUCT, elements: std::ptr::null_mut() });
                let mut els: Vec<*mut FfiType> = vec![item; n.min(4096)];
                els.push(std::ptr::null_mut());
                ch.elements = Box::leak(els.into_boxed_slice()).as_mut_ptr();
                let p = &mut *ch as *mut FfiType;
                children.push(ch);
                if n == 0 {
                    continue;
                }
                p
            }
        };
        if fd.bits.is_some() {
            flags |= TYPEFLAG_HASBITFIELD;
            if last_bit_storage == Some(fd.byte_offset) {
                continue;
            }
            last_bit_storage = Some(fd.byte_offset);
        }
        if is_struct {
            elements.push(ft);
        } else {
            let sz = fi.size;
            if biggest.map_or(true, |(s, _)| sz > s) {
                biggest = Some((sz, ft));
            }
        }
    }
    if !is_struct {
        if let Some((_, ft)) = biggest {
            elements.push(ft);
        }
    }
    elements.push(std::ptr::null_mut());

    // install the fields as class attributes
    for f in &cfields {
        let fd = field(f);
        let ts = unsafe { (*rpy_runtime::types::typeobject::type_type()).tp_setattro.unwrap() };
        if unsafe { ts(cls.as_ptr(), fd.name.as_ptr(), f.as_ptr()) } < 0 {
            return Err(PyErr);
        }
    }

    let info = stginfo(tp).unwrap();
    info.size = size;
    info.align = align;
    info.length = base_length + cfields.len() as isize;
    info.ffi_children = children;
    info.ffi_elements = elements;
    *info.ffi = FfiType { size: size as usize, alignment: align.clamp(1, u16::MAX as isize) as u16, type_: libffi::FFI_TYPE_STRUCT, elements: std::ptr::null_mut() };
    info.ffi.elements = info.ffi_elements.as_mut_ptr();
    info.format = if is_struct { fmt } else { Some("B".to_string()) };
    info.shape.clear();
    info.dict_flags = (info.dict_flags & !(TYPEFLAG_HASPOINTER | TYPEFLAG_HASUNION | TYPEFLAG_HASBITFIELD)) | flags | DICTFLAG_FINAL;

    if let Some(anon) = own_dict_item(tp, "_anonymous_") {
        make_anon_fields(cls, &anon)?;
    }
    Ok(())
}

/// Expose the fields of anonymous sub-structures directly on `cls`.
fn make_anon_fields(cls: &PyObject, anon: &PyObject) -> PyResult<()> {
    let names = match from_c(unsafe { capi::PySequence_Fast(anon.as_ptr(), cs!("_anonymous_ must be a sequence")) }) {
        Ok(n) => n,
        Err(e) => return Err(e),
    };
    let names = rpy_runtime::abstract_::collect_iterable(&names)?;
    let tp = cls.as_ptr() as *mut PyTypeObject;
    for nm in &names {
        let descr = match own_dict_item(tp, &str_of(nm)) {
            Some(d) if is_cfield(&d) => d,
            _ => raise!(AttributeError, "'{}' is specified in _anonymous_ but not in _fields_", str_of(nm)),
        };
        field(&descr).anonymous = true;
        let fd = field(&descr);
        make_fields(cls, &fd.proto.clone(), fd.index, fd.byte_offset)?;
    }
    Ok(())
}

fn make_fields(cls: &PyObject, anon_type: &PyObject, index: isize, offset: isize) -> PyResult<()> {
    let fields = match getattr_opt(anon_type, "_fields_")? {
        Some(f) => f,
        None => return Ok(()),
    };
    for item in rpy_runtime::abstract_::collect_iterable(&fields)? {
        let fname = rpy_runtime::abstract_::getitem(&item, &new_int(0))?;
        let fdescr = getattr(anon_type, &str_of(&fname))?;
        if !is_cfield(&fdescr) {
            raise!(TypeError, "unexpected type");
        }
        let fd = field(&fdescr);
        if fd.anonymous {
            make_fields(cls, &fd.proto.clone(), index + fd.index, offset + fd.byte_offset)?;
            continue;
        }
        let bits = fd.bits.map(|(a, b)| (a as isize, b as isize));
        let nf = new_cfield(&fd.name, &fd.proto, fd.byte_size, offset + fd.byte_offset, index + fd.index, bits)?;
        let ts = unsafe { (*rpy_runtime::types::typeobject::type_type()).tp_setattro.unwrap() };
        if unsafe { ts(cls.as_ptr(), fd.name.as_ptr(), nf.as_ptr()) } < 0 {
            return Err(PyErr);
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ instances

unsafe extern "C" fn struct_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let a = rpy_runtime::modules::fncommon::targs(args);
        let kwd = rpy_runtime::modules::fncommon::opt(kw);
        int_into_c(struct_init_impl(&*o, a, kwd))
    }
}

fn init_pos_args(o: &PyObject, tp: *mut PyTypeObject, args: &[&PyObject], kw: Option<&PyObject>, index: usize) -> PyResult<usize> {
    let mut index = index;
    let base = unsafe { (*tp).tp_base };
    if let Some(bi) = stginfo(base) {
        if bi.length > 0 {
            index = init_pos_args(o, base, args, kw, index)?;
        }
    }
    let fields = match own_dict_item(tp, "_fields_") {
        Some(f) => f,
        None => return Ok(index),
    };
    let items = rpy_runtime::abstract_::collect_iterable(&fields)?;
    let mut i = 0;
    while i < items.len() && index + i < args.len() {
        let name = rpy_runtime::abstract_::getitem(&items[i], &new_int(0))?;
        let a = args[index + i];
        if let Some(k) = kw {
            if dict::get_item_str(k, &name).is_some() {
                raise!(TypeError, "duplicate values for field {}", repr(&name)?);
            }
        }
        rpy_runtime::abstract_::setattr(o, &name, a)?;
        i += 1;
    }
    Ok(index + items.len())
}

fn struct_init_impl(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult<()> {
    if !args.is_empty() {
        let n = init_pos_args(o, o.type_ptr(), args, kw, 0)?;
        if n < args.len() {
            raise!(TypeError, "too many initializers");
        }
    }
    if let Some(k) = kw {
        for (key, v) in dict::items_vec(k) {
            rpy_runtime::abstract_::setattr(o, &key, &v)?;
        }
    }
    Ok(())
}

pub fn init_types() -> PyResult<()> {
    unsafe {
        for (m, name, doc) in [
            (struct_meta(), c"_ctypes.PyCStructType", c"Metaclass of Structure and Union types."),
            (union_meta(), c"_ctypes.UnionType", c"Metaclass of Structure and Union types."),
        ] {
            init_meta(m, name, ctype_type());
            (*m).tp_init = Some(struct_meta_init);
            (*m).tp_setattro = Some(struct_meta_setattro);
            (*m).tp_methods = meta_methods(rpy_runtime::mdefs![("from_param", O, ct_from_param)], vec![]);
            (*m).tp_doc = doc.as_ptr();
            set_meta_number(m);
            rpy_runtime::types::typeobject::type_ready(m)?;
        }
        for (t, name, meta) in [(struct_type(), c"_ctypes.Structure", struct_meta()), (union_type(), c"_ctypes.Union", union_meta())] {
            init_base_type(t, name, meta, std::mem::size_of::<CDataObject>());
            (*t).tp_init = Some(struct_init);
            rpy_runtime::types::typeobject::type_ready(t)?;
        }
    }
    let _ = tuple::empty_tuple;
    Ok(())
}
