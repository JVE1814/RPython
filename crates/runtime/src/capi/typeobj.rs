//! Type objects: PyType_Ready for static C types, PyType_FromSpec & friends with all
//! slot ids (slots_generated.h numbering), PyType_GetSlot, module lookup, names.

use super::*;
use crate::types::module::PyModuleObject;
use crate::types::typeobject::{self as to, as_type, generic_alloc, is_heap_type, is_type, type_obj, type_ref, type_type};
use crate::types::{dict, none, str as pystr, tuple};
use std::ffi::c_void;

pub(crate) const fn align_up(x: Py_ssize_t) -> Py_ssize_t {
    (x + 15) & !15
}

/// `_PyObject_NextNotImplemented`: tp_iternext of types that are not iterators.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_NextNotImplemented(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let _ = type_error(&format!("'{}' object is not iterable", tname(&*o)));
        std::ptr::null_mut()
    }
}
pub(crate) use _PyObject_NextNotImplemented as not_implemented_iternext;

// ---------------------------------------------------------------- basics

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_Ready(tp: *mut PyTypeObject) -> c_int {
    unsafe {
        if (*tp).tp_flags & Py_TPFLAGS_READY != 0 {
            return 0;
        }
        let disallow = !is_heap_type(tp)
            && (*tp).tp_new.is_none()
            && ((*tp).tp_base.is_null() || (*tp).tp_base == to::object_type())
            && tp != to::object_type();
        if disallow {
            (*tp).tp_flags |= Py_TPFLAGS_DISALLOW_INSTANTIATION;
        }
        if (*tp).tp_flags & Py_TPFLAGS_HAVE_GC != 0 && (*tp).tp_free.is_none() {
            (*tp).tp_free = Some(super::mem::PyObject_GC_Del);
        }
        if (*tp).tp_alloc.is_none() {
            (*tp).tp_alloc = Some(generic_alloc);
        }
        let prev = to::SHARE_BASE_SUBSTRUCTS.with(|c| c.replace(true));
        let r = to::type_ready(tp);
        to::SHARE_BASE_SUBSTRUCTS.with(|c| c.set(prev));
        if (*tp).tp_flags & Py_TPFLAGS_DISALLOW_INSTANTIATION != 0 {
            (*tp).tp_new = None;
            if !(*tp).tp_dict.is_null() {
                let _ = dict::del_item(&*(*tp).tp_dict, crate::name!("__new__"));
            }
        }
        // vectorcall flag for types defining tp_vectorcall_offset
        ret_i(r)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GenericAlloc(tp: *mut PyTypeObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe { generic_alloc(tp, n) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GenericNew(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        alloc(tp, 0)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_IsSubtype(a: *mut PyTypeObject, b: *mut PyTypeObject) -> c_int {
    to::is_subtype(a, b) as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetFlags(t: *mut PyTypeObject) -> std::ffi::c_ulong {
    unsafe { (*t).tp_flags }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_Modified(t: *mut PyTypeObject) {
    crate::types::slots::type_modified(t)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_ClearCache() -> std::ffi::c_uint {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Type_AssignVersionTag(_t: *mut PyTypeObject) -> c_int {
    1
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetDict(t: *mut PyTypeObject) -> *mut PyObject {
    unsafe {
        let d = (*t).tp_dict;
        xincref(d);
        d
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyType_Lookup(t: *mut PyTypeObject, n: *mut PyObject) -> *mut PyObject {
    unsafe {
        match to::lookup(t, &*n) {
            Some(v) => {
                let p = v.as_ptr();
                drop(v); // borrowed (kept alive by the type's dict)
                p
            }
            None => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyType_LookupRef(t: *mut PyTypeObject, n: *mut PyObject) -> *mut PyObject {
    unsafe { ret_opt(to::lookup(t, &*n)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyType_Name(t: *mut PyTypeObject) -> *const c_char {
    unsafe {
        let n = (*t).tp_name;
        let s = CStr::from_ptr(n).to_bytes();
        match s.iter().rposition(|&c| c == b'.') {
            Some(i) => n.add(i + 1),
            None => n,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetName(t: *mut PyTypeObject) -> *mut PyObject {
    unsafe {
        if is_heap_type(t) {
            let h = to::heap(t);
            if !(*h).ht_name.is_null() {
                return (*(*h).ht_name).to_ref().into_raw();
            }
        }
        pystr::new_str(&to::type_name(&*t)).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetQualName(t: *mut PyTypeObject) -> *mut PyObject {
    unsafe { pystr::new_str(&to::type_qualname(&*t)).into_raw() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetFullyQualifiedName(t: *mut PyTypeObject) -> *mut PyObject {
    unsafe { pystr::new_str(&to::type_fqname(t)).into_raw() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetModuleName(t: *mut PyTypeObject) -> *mut PyObject {
    unsafe {
        if is_heap_type(t) {
            return ret(crate::abstract_::getattr_str(type_obj(t), "__module__"));
        }
        pystr::new_str(&to::type_module(t).unwrap_or_else(|| "builtins".into())).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_Freeze(t: *mut PyTypeObject) -> c_int {
    unsafe {
        (*t).tp_flags |= Py_TPFLAGS_IMMUTABLETYPE;
        PyType_Modified(t);
        0
    }
}

static TYPE_WATCHERS: std::sync::Mutex<[usize; 8]> = std::sync::Mutex::new([0; 8]);
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_AddWatcher(cb: *mut c_void) -> c_int {
    let mut w = TYPE_WATCHERS.lock().unwrap();
    for (i, s) in w.iter_mut().enumerate() {
        if *s == 0 {
            *s = cb as usize;
            return i as c_int;
        }
    }
    let _ = crate::err!(RuntimeError, "no more type watcher IDs available");
    -1
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_ClearWatcher(id: c_int) -> c_int {
    let mut w = TYPE_WATCHERS.lock().unwrap();
    if !(0..8).contains(&id) || w[id as usize] == 0 {
        let _ = value_error(&format!("No type watcher set for ID {}", id));
        return -1;
    }
    w[id as usize] = 0;
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_Watch(id: c_int, _t: *mut PyObject) -> c_int {
    if !(0..8).contains(&id) {
        let _ = value_error(&format!("Invalid type watcher ID {}", id));
        return -1;
    }
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_Unwatch(id: c_int, t: *mut PyObject) -> c_int {
    unsafe { PyType_Watch(id, t) }
}

// ---------------------------------------------------------------- module association

unsafe fn ht_module(t: *mut PyTypeObject) -> *mut PyObject {
    unsafe { if is_heap_type(t) { (*to::heap(t)).ht_module } else { std::ptr::null_mut() } }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetModule(t: *mut PyTypeObject) -> *mut PyObject {
    unsafe {
        if !is_heap_type(t) {
            let _ = type_error(&format!("PyType_GetModule: Type '{}' is not a heap type", to::type_name(&*t)));
            return std::ptr::null_mut();
        }
        let m = ht_module(t);
        if m.is_null() {
            let _ = type_error(&format!("PyType_GetModule: Type '{}' has no associated module", to::type_name(&*t)));
        }
        m
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetModule_DuringGC(t: *mut PyTypeObject) -> *mut PyObject {
    unsafe { ht_module(t) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetModuleState(t: *mut PyTypeObject) -> *mut c_void {
    unsafe {
        let m = PyType_GetModule(t);
        if m.is_null() {
            return std::ptr::null_mut();
        }
        (*(m as *mut PyModuleObject)).md_state
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetModuleState_DuringGC(t: *mut PyTypeObject) -> *mut c_void {
    unsafe {
        let m = ht_module(t);
        if m.is_null() { std::ptr::null_mut() } else { (*(m as *mut PyModuleObject)).md_state }
    }
}
unsafe fn find_module_in_mro(t: *mut PyTypeObject, pred: &dyn Fn(*mut PyModuleObject) -> bool) -> *mut PyObject {
    unsafe {
        let mro = (*t).tp_mro;
        let list: Vec<*mut PyTypeObject> = if mro.is_null() {
            vec![t]
        } else {
            tuple::tuple_items(&*mro).iter().map(|x| as_type(x)).collect()
        };
        for b in list {
            let m = ht_module(b);
            if !m.is_null() && pred(m as *mut PyModuleObject) {
                return m;
            }
        }
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetModuleByDef(t: *mut PyTypeObject, def: *mut PyModuleDef) -> *mut PyObject {
    unsafe {
        let m = find_module_in_mro(t, &|m| (*m).md_def == def);
        if m.is_null() {
            let _ = type_error(&format!("PyType_GetModuleByDef: No superclass of '{}' has the given module", to::type_name(&*t)));
        }
        m
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetModuleByToken_DuringGC(t: *mut PyTypeObject, token: *const c_void) -> *mut PyObject {
    unsafe { find_module_in_mro(t, &|m| super::module::module_token(m) == token as *mut c_void) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetModuleByToken(t: *mut PyTypeObject, token: *const c_void) -> *mut PyObject {
    unsafe {
        let m = PyType_GetModuleByToken_DuringGC(t, token);
        if m.is_null() {
            let _ = type_error(&format!("PyType_GetModuleByToken: No superclass of '{}' has the given module", to::type_name(&*t)));
            return m;
        }
        (*m).to_ref().into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetBaseByToken_DuringGC(t: *mut PyTypeObject, token: *mut c_void, result: *mut *mut PyTypeObject) -> c_int {
    unsafe {
        if !result.is_null() {
            *result = std::ptr::null_mut();
        }
        if token.is_null() {
            return 0;
        }
        let mro = (*t).tp_mro;
        let list: Vec<*mut PyTypeObject> = if mro.is_null() {
            let mut v = vec![];
            let mut b = t;
            while !b.is_null() {
                v.push(b);
                b = (*b).tp_base;
            }
            v
        } else {
            tuple::tuple_items(&*mro).iter().map(|x| as_type(x)).collect()
        };
        for b in list {
            if is_heap_type(b) && (*to::heap(b)).ht_token == token {
                if !result.is_null() {
                    *result = b;
                }
                return 1;
            }
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetBaseByToken(t: *mut PyTypeObject, token: *mut c_void, result: *mut *mut PyTypeObject) -> c_int {
    unsafe {
        if t.is_null() || !is_type(&*(t as *mut PyObject)) {
            if !result.is_null() {
                *result = std::ptr::null_mut();
            }
            let _ = type_error(&format!(
                "expected a type, got a '{}' object",
                if t.is_null() { "NULL".into() } else { tname(&*(t as *mut PyObject)) }
            ));
            return -1;
        }
        let r = PyType_GetBaseByToken_DuringGC(t, token, result);
        if r == 1 && !result.is_null() {
            incref(*result as *mut PyObject);
        }
        r
    }
}

// ---------------------------------------------------------------- slots

/// Address of the slot field with the given id inside a (heap) type object.
unsafe fn slot_addr(tp: *mut PyTypeObject, id: c_int) -> Option<*mut *mut c_void> {
    unsafe {
        let t = &mut *tp;
        macro_rules! f {
            ($e:expr) => {
                Some((&raw mut $e) as *mut *mut c_void)
            };
        }
        macro_rules! sub {
            ($p:expr, $f:ident) => {
                if $p.is_null() { None } else { Some((&raw mut (*$p).$f) as *mut *mut c_void) }
            };
        }
        let nb = t.tp_as_number;
        let sq = t.tp_as_sequence;
        let mp = t.tp_as_mapping;
        let am = t.tp_as_async;
        let bf = t.tp_as_buffer;
        match id {
            1 | 88 => sub!(bf, bf_getbuffer),
            2 | 89 => sub!(bf, bf_releasebuffer),
            3 | 90 => sub!(mp, mp_ass_subscript),
            4 | 91 => sub!(mp, mp_length),
            5 => sub!(mp, mp_subscript),
            6 => sub!(nb, nb_absolute),
            7 => sub!(nb, nb_add),
            8 => sub!(nb, nb_and),
            9 => sub!(nb, nb_bool),
            10 => sub!(nb, nb_divmod),
            11 => sub!(nb, nb_float),
            12 => sub!(nb, nb_floor_divide),
            13 => sub!(nb, nb_index),
            14 => sub!(nb, nb_inplace_add),
            15 => sub!(nb, nb_inplace_and),
            16 => sub!(nb, nb_inplace_floor_divide),
            17 => sub!(nb, nb_inplace_lshift),
            18 => sub!(nb, nb_inplace_multiply),
            19 => sub!(nb, nb_inplace_or),
            20 => sub!(nb, nb_inplace_power),
            21 => sub!(nb, nb_inplace_remainder),
            22 => sub!(nb, nb_inplace_rshift),
            23 => sub!(nb, nb_inplace_subtract),
            24 => sub!(nb, nb_inplace_true_divide),
            25 => sub!(nb, nb_inplace_xor),
            26 => sub!(nb, nb_int),
            27 => sub!(nb, nb_invert),
            28 => sub!(nb, nb_lshift),
            29 => sub!(nb, nb_multiply),
            30 => sub!(nb, nb_negative),
            31 => sub!(nb, nb_or),
            32 => sub!(nb, nb_positive),
            33 => sub!(nb, nb_power),
            34 => sub!(nb, nb_remainder),
            35 => sub!(nb, nb_rshift),
            36 => sub!(nb, nb_subtract),
            37 => sub!(nb, nb_true_divide),
            38 => sub!(nb, nb_xor),
            39 => sub!(sq, sq_ass_item),
            40 => sub!(sq, sq_concat),
            41 => sub!(sq, sq_contains),
            42 => sub!(sq, sq_inplace_concat),
            43 => sub!(sq, sq_inplace_repeat),
            44 => sub!(sq, sq_item),
            45 => sub!(sq, sq_length),
            46 => sub!(sq, sq_repeat),
            47 => f!(t.tp_alloc),
            48 => f!(t.tp_base),
            49 => f!(t.tp_bases),
            50 => f!(t.tp_call),
            51 => f!(t.tp_clear),
            52 => f!(t.tp_dealloc),
            53 => f!(t.tp_del),
            54 => f!(t.tp_descr_get),
            55 => f!(t.tp_descr_set),
            56 => f!(t.tp_doc),
            57 => f!(t.tp_getattr),
            58 => f!(t.tp_getattro),
            59 => f!(t.tp_hash),
            60 => f!(t.tp_init),
            61 => f!(t.tp_is_gc),
            62 => f!(t.tp_iter),
            63 => f!(t.tp_iternext),
            64 => f!(t.tp_methods),
            65 => f!(t.tp_new),
            66 => f!(t.tp_repr),
            67 => f!(t.tp_richcompare),
            68 => f!(t.tp_setattr),
            69 => f!(t.tp_setattro),
            70 => f!(t.tp_str),
            71 => f!(t.tp_traverse),
            72 => f!(t.tp_members),
            73 => f!(t.tp_getset),
            74 => f!(t.tp_free),
            75 => sub!(nb, nb_matrix_multiply),
            76 => sub!(nb, nb_inplace_matrix_multiply),
            77 => sub!(am, am_await),
            78 => sub!(am, am_aiter),
            79 => sub!(am, am_anext),
            80 => f!(t.tp_finalize),
            81 => sub!(am, am_send),
            82 => f!(t.tp_vectorcall),
            83 => {
                if is_heap_type(tp) {
                    Some((&raw mut (*to::heap(tp)).ht_token) as *mut *mut c_void)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

const MAX_SLOT: c_int = 110;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetSlot(t: *mut PyTypeObject, id: c_int) -> *mut c_void {
    unsafe {
        if id <= 0 || id > MAX_SLOT || (84..=87).contains(&id) || id > 91 {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if id == 83 && !is_heap_type(t) {
            return std::ptr::null_mut();
        }
        match slot_addr(t, id) {
            Some(p) => *p,
            None => std::ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------- FromSpec

#[repr(C)]
pub struct PySlot {
    pub sl_id: u16,
    pub sl_flags: u16,
    pub sl_reserved: u32,
    pub sl_ptr: *mut c_void,
}

/// Normalised description of a heap type to create (from a PyType_Spec or PySlot array).
pub(crate) struct TypeDesc {
    pub name: String,
    pub basicsize: isize,
    pub extra_basicsize: isize,
    pub itemsize: isize,
    pub flags: std::ffi::c_ulong,
    pub slots: Vec<(c_int, *mut c_void)>,
    pub metaclass: *mut PyTypeObject,
    pub module: *mut PyObject,
    pub bases: *mut PyObject,
}

unsafe fn members_special(m: *mut PyMemberDef, tp: *mut PyTypeObject, data_off: isize) -> *mut PyMemberDef {
    unsafe {
        // copy the table, resolving Py_RELATIVE_OFFSET and the special offset members
        let mut v: Vec<PyMemberDef> = Vec::new();
        let mut p = m;
        while !(*p).name.is_null() {
            let mut d = PyMemberDef { name: (*p).name, type_code: (*p).type_code, offset: (*p).offset, flags: (*p).flags, doc: (*p).doc };
            if d.flags & Py_RELATIVE_OFFSET != 0 {
                d.offset += data_off;
                d.flags &= !Py_RELATIVE_OFFSET;
            }
            let name = CStr::from_ptr(d.name).to_bytes();
            match name {
                b"__weaklistoffset__" => (*tp).tp_weaklistoffset = d.offset,
                b"__dictoffset__" => (*tp).tp_dictoffset = d.offset,
                b"__vectorcalloffset__" => (*tp).tp_vectorcall_offset = d.offset,
                _ => {}
            }
            v.push(d);
            p = p.add(1);
        }
        v.push(std::mem::zeroed());
        Box::leak(v.into_boxed_slice()).as_mut_ptr()
    }
}

pub(crate) unsafe fn type_from_desc(desc: TypeDesc, token: *mut c_void) -> PyResult {
    unsafe {
        // bases
        let mut base_slot: *mut c_void = std::ptr::null_mut();
        let mut bases_slot: *mut c_void = std::ptr::null_mut();
        let mut doc: *const c_char = std::ptr::null();
        let mut members: *mut PyMemberDef = std::ptr::null_mut();
        let mut tok: *mut c_void = token;
        let mut has_token_slot = false;
        for &(id, p) in &desc.slots {
            match id {
                48 => base_slot = p,
                49 => bases_slot = p,
                56 => doc = p as *const c_char,
                72 => members = p as *mut PyMemberDef,
                83 => {
                    has_token_slot = true;
                    if !p.is_null() {
                        tok = p;
                    }
                }
                _ => {}
            }
        }
        if !has_token_slot {
            tok = std::ptr::null_mut();
        }
        let bases: PyRef = if !desc.bases.is_null() {
            if tuple::is_tuple(&*desc.bases) { (*desc.bases).to_ref() } else { tuple::new_tuple(vec![(*desc.bases).to_ref()]) }
        } else if !bases_slot.is_null() {
            (*(bases_slot as *mut PyObject)).to_ref()
        } else if !base_slot.is_null() {
            tuple::new_tuple(vec![(*(base_slot as *mut PyObject)).to_ref()])
        } else {
            tuple::new_tuple(vec![type_ref(to::object_type())])
        };
        // the best base
        let mut base: *mut PyTypeObject = std::ptr::null_mut();
        for b in tuple::tuple_items(&bases) {
            if !is_type(b) {
                return Err(type_error("bases must be types"));
            }
            let bt = as_type(b);
            if (*bt).tp_flags & Py_TPFLAGS_READY == 0 {
                to::type_ready(bt)?;
            }
            if (*bt).tp_flags & Py_TPFLAGS_BASETYPE == 0 {
                return Err(type_error(&format!("type '{}' is not an acceptable base type", to::type_fqname(bt))));
            }
            if base.is_null() || to::is_subtype(to::solid_base(bt), to::solid_base(base)) {
                base = bt;
            }
        }
        // metaclass
        let mut meta = if desc.metaclass.is_null() { type_type() } else { desc.metaclass };
        for b in tuple::tuple_items(&bases) {
            let bt = b.type_ptr();
            if to::is_subtype(meta, bt) {
                continue;
            }
            if to::is_subtype(bt, meta) {
                meta = bt;
                continue;
            }
            return Err(type_error("metaclass conflict: the metaclass of a derived class must be a (non-strict) subclass of the metaclasses of all its bases"));
        }
        if (*meta).tp_new.is_some() && (*meta).tp_new != (*type_type()).tp_new {
            return Err(type_error("Metaclasses with custom tp_new are not supported."));
        }
        // sizes
        let base_size = (*base).tp_basicsize;
        let (basicsize, data_off) = if desc.extra_basicsize > 0 {
            let off = align_up(base_size);
            (off + desc.extra_basicsize, off)
        } else if desc.basicsize < 0 {
            let off = align_up(base_size);
            (off - desc.basicsize, off)
        } else if desc.basicsize == 0 {
            (base_size, base_size)
        } else {
            (desc.basicsize, align_up(base_size))
        };
        let itemsize = if desc.itemsize != 0 { desc.itemsize } else { (*base).tp_itemsize };

        let tp_obj = generic_alloc(meta, 0);
        if tp_obj.is_null() {
            return Err(PyErr);
        }
        let typ = PyRef::from_raw(tp_obj);
        let tp = tp_obj as *mut PyTypeObject;
        let h = to::heap(tp);
        (*tp).tp_flags = desc.flags | Py_TPFLAGS_HEAPTYPE;
        (*tp).tp_as_async = &raw mut (*h).as_async;
        (*tp).tp_as_number = &raw mut (*h).as_number;
        (*tp).tp_as_sequence = &raw mut (*h).as_sequence;
        (*tp).tp_as_mapping = &raw mut (*h).as_mapping;
        (*tp).tp_as_buffer = &raw mut (*h).as_buffer;
        // names
        let full = desc.name.clone();
        let (modname, short) = match full.rfind('.') {
            Some(i) => (Some(full[..i].to_string()), full[i + 1..].to_string()),
            None => (None, full.clone()),
        };
        (*h).ht_name = pystr::new_str(&short).into_raw();
        (*h).ht_qualname = pystr::new_str(&short).into_raw();
        let cname = leak_cstring(&full);
        (*h)._ht_tpname = cname as *mut c_char;
        (*tp).tp_name = cname;
        (*tp).tp_basicsize = basicsize;
        (*tp).tp_itemsize = itemsize;
        incref(base as *mut PyObject);
        (*tp).tp_base = base;
        (*tp).tp_bases = bases.clone().into_raw();
        if !desc.module.is_null() {
            (*h).ht_module = (*desc.module).to_ref().into_raw();
        }
        (*h).ht_token = tok;
        // apply slots
        for &(id, p) in &desc.slots {
            match id {
                48 | 49 | 56 | 72 | 83 => {}
                _ => {
                    if let Some(addr) = slot_addr(tp, id) {
                        *addr = p;
                    } else if !(84..=87).contains(&id) && id != 0 {
                        return Err(crate::errors::new_err(crate::types::exceptions::RuntimeError(), &format!("invalid slot offset")));
                    }
                }
            }
        }
        if !members.is_null() {
            (*tp).tp_members = members_special(members, tp, data_off);
        }
        if (*tp).tp_vectorcall_offset != 0 {
            (*tp).tp_flags |= Py_TPFLAGS_HAVE_VECTORCALL;
        }
        if !doc.is_null() {
            let s = CStr::from_ptr(doc).to_string_lossy().into_owned();
            (*tp).tp_doc = leak_cstring(&s);
        }
        if (*tp).tp_dealloc.is_none() {
            (*tp).tp_dealloc = Some(to::subtype_dealloc);
        }
        if (*tp).tp_flags & Py_TPFLAGS_HAVE_GC != 0 && (*tp).tp_free.is_none() {
            (*tp).tp_free = Some(super::mem::PyObject_GC_Del);
        }
        // dict with __module__ / __doc__
        let d = dict::new_dict();
        (*tp).tp_dict = d.into_raw();
        let disallow = (*tp).tp_flags & Py_TPFLAGS_DISALLOW_INSTANTIATION != 0;
        to::type_ready(tp)?;
        // the module from the dotted name, unless the type defines a `__module__` member or
        // getset for its instances (Cython's function type: each function has its own)
        if let Some(m) = &modname {
            let d = &*(*tp).tp_dict;
            if dict::get_item_str(d, crate::name!("__module__")).is_none() {
                dict::set_item_str(d, "__module__", &pystr::new_str(m))?;
            }
        }
        if disallow {
            (*tp).tp_new = None;
            let _ = dict::del_item(&*(*tp).tp_dict, crate::name!("__new__"));
        }
        Ok(typ)
    }
}

unsafe fn desc_from_spec(meta: *mut PyTypeObject, module: *mut PyObject, spec: *mut PyType_Spec, bases: *mut PyObject) -> TypeDesc {
    unsafe {
        let mut slots = Vec::new();
        let mut s = (*spec).slots;
        while !s.is_null() && (*s).slot != 0 {
            slots.push(((*s).slot, (*s).pfunc));
            s = s.add(1);
        }
        TypeDesc {
            name: cs((*spec).name).into_owned(),
            basicsize: (*spec).basicsize as isize,
            extra_basicsize: 0,
            itemsize: (*spec).itemsize as isize,
            flags: (*spec).flags as std::ffi::c_ulong,
            slots,
            metaclass: meta,
            module,
            bases,
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_FromMetaclass(meta: *mut PyTypeObject, module: *mut PyObject, spec: *mut PyType_Spec, bases: *mut PyObject) -> *mut PyObject {
    unsafe {
        let d = desc_from_spec(meta, module, spec, bases);
        ret(type_from_desc(d, spec as *mut c_void))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_FromModuleAndSpec(module: *mut PyObject, spec: *mut PyType_Spec, bases: *mut PyObject) -> *mut PyObject {
    unsafe { PyType_FromMetaclass(std::ptr::null_mut(), module, spec, bases) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_FromSpecWithBases(spec: *mut PyType_Spec, bases: *mut PyObject) -> *mut PyObject {
    unsafe { PyType_FromMetaclass(std::ptr::null_mut(), std::ptr::null_mut(), spec, bases) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_FromSpec(spec: *mut PyType_Spec) -> *mut PyObject {
    unsafe { PyType_FromMetaclass(std::ptr::null_mut(), std::ptr::null_mut(), spec, std::ptr::null_mut()) }
}

/// Flatten a PySlot array (following Py_slot_subslots / Py_tp_slots / Py_mod_slots).
pub(crate) unsafe fn flatten_slots(slots: *const PySlot, out: &mut Vec<(u16, u16, *mut c_void)>) {
    unsafe {
        let mut p = slots;
        while !p.is_null() && (*p).sl_id != 0 {
            match (*p).sl_id {
                92 => flatten_slots((*p).sl_ptr as *const PySlot, out),
                93 => {
                    // array of old-style PyType_Slot
                    let mut s = (*p).sl_ptr as *const PyType_Slot;
                    while !s.is_null() && (*s).slot != 0 {
                        out.push(((*s).slot as u16, 0, (*s).pfunc));
                        s = s.add(1);
                    }
                }
                94 => {
                    let mut s = (*p).sl_ptr as *const PyModuleDef_Slot;
                    while !s.is_null() && (*s).slot != 0 {
                        // old module slot ids 1..4 map to 84..87
                        let id = if (1..=4).contains(&(*s).slot) { (*s).slot + 83 } else { (*s).slot };
                        out.push((id as u16, 0, (*s).value));
                        s = s.add(1);
                    }
                }
                id => out.push((id, (*p).sl_flags, (*p).sl_ptr)),
            }
            p = p.add(1);
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_FromSlots(slots: *const PySlot) -> *mut PyObject {
    unsafe {
        let mut flat = Vec::new();
        flatten_slots(slots, &mut flat);
        let mut desc = TypeDesc {
            name: String::new(),
            basicsize: 0,
            extra_basicsize: 0,
            itemsize: 0,
            flags: Py_TPFLAGS_DEFAULT,
            slots: Vec::new(),
            metaclass: std::ptr::null_mut(),
            module: std::ptr::null_mut(),
            bases: std::ptr::null_mut(),
        };
        let mut have_name = false;
        for (id, flags, p) in flat {
            match id {
                95 => {
                    desc.name = cs(p as *const c_char).into_owned();
                    have_name = true;
                }
                96 => desc.basicsize = p as isize,
                97 => desc.extra_basicsize = p as isize,
                98 => desc.itemsize = p as isize,
                99 => desc.flags = p as usize as std::ffi::c_ulong,
                107 => desc.metaclass = p as *mut PyTypeObject,
                108 => desc.module = p as *mut PyObject,
                1..=83 | 88..=91 => desc.slots.push((id as c_int, p)),
                _ => {
                    if flags & 1 == 0 {
                        let _ = system_error(&format!("invalid slot ID {} for a type", id));
                        return std::ptr::null_mut();
                    }
                }
            }
        }
        if !have_name {
            let _ = system_error("Py_tp_name is required");
            return std::ptr::null_mut();
        }
        ret(type_from_desc(desc, std::ptr::null_mut()))
    }
}

#[allow(dead_code)]
fn _unused() -> PyRef {
    none::none_ref()
}
