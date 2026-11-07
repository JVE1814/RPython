//! Generic "struct sequence" types (os.stat_result, time.struct_time, sys.flags, ...).
//!
//! Layout follows CPython: a tuple subclass whose instances allocate `n_fields` item
//! slots; `ob_size` is `n_sequence_fields` (the visible tuple part) and the remaining
//! slots hold the hidden (attribute-only) fields. Named fields are exposed as read-only
//! member descriptors. `n_fields`, `n_sequence_fields`, `n_unnamed_fields` and
//! `__match_args__` live in the type dict.
//!
//! Rust API:
//! ```ignore
//! static FIELDS: &[Field] = &[Field::new("tm_year", "year"), ..., Field::unnamed("...")];
//! let tp = structseq::new_type(&Desc { name: "time.struct_time", doc: "...", fields: FIELDS, n_in_sequence: 9 })?;
//! let obj = structseq::new_instance(tp, vec![...n_fields values...]);
//! ```

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{dict, int, none, str as pystr, tuple};
use std::ffi::{CStr, c_char, c_int, c_void};

/// Name marking an unnamed field (only accessible by index).
pub const UNNAMED: &str = "unnamed field";

#[unsafe(no_mangle)]
pub static PyStructSequence_UnnamedField: &CStr = c"unnamed field";

#[derive(Clone, Copy)]
pub struct Field {
    pub name: &'static str,
    pub doc: &'static str,
}
impl Field {
    pub const fn new(name: &'static str, doc: &'static str) -> Field {
        Field { name, doc }
    }
    pub const fn unnamed(doc: &'static str) -> Field {
        Field { name: UNNAMED, doc }
    }
    fn is_unnamed(&self) -> bool {
        self.name == UNNAMED
    }
}

pub struct Desc<'a> {
    /// Fully qualified name, e.g. "os.stat_result".
    pub name: &'a str,
    pub doc: &'a str,
    pub fields: &'a [Field],
    pub n_in_sequence: usize,
}

const HEADER: usize = std::mem::offset_of!(PyTupleObject, ob_item);
const PTR: usize = std::mem::size_of::<*mut PyObject>();

/// Number of hidden slots of instances of `tp` (computed from the basic size so that
/// it also works while the type is being torn down).
#[inline]
fn hidden_count(tp: *mut PyTypeObject) -> usize {
    unsafe { ((*tp).tp_basicsize as usize).saturating_sub(HEADER) / PTR }
}

fn type_int(tp: *mut PyTypeObject, nm: &str) -> PyResult<usize> {
    match lookup_str(tp, nm) {
        Some(v) => Ok(ab::as_i64(&v)? as usize),
        None => raise!(TypeError, "Missed attribute '{}' of type {}", nm, type_name(unsafe { &*tp })),
    }
}

pub fn n_fields(tp: *mut PyTypeObject) -> PyResult<usize> {
    type_int(tp, "n_fields")
}
pub fn n_sequence_fields(tp: *mut PyTypeObject) -> PyResult<usize> {
    type_int(tp, "n_sequence_fields")
}
pub fn n_unnamed_fields(tp: *mut PyTypeObject) -> PyResult<usize> {
    type_int(tp, "n_unnamed_fields")
}

/// Total number of item slots of a structseq instance.
#[inline]
pub fn instance_slots(o: &PyObject) -> usize {
    var_size(o) as usize + hidden_count(o.type_ptr())
}

/// Borrow item `i` (visible or hidden). Panics if out of range.
pub fn get_item(o: &PyObject, i: usize) -> &PyObject {
    assert!(i < instance_slots(o));
    unsafe { &**tuple::item_ptr(o.as_ptr()).add(i) }
}

/// Allocate an instance with every slot set to None-less NULL; caller fills via `set_item`.
unsafe fn alloc_instance(tp: *mut PyTypeObject, visible: usize) -> *mut PyObject {
    unsafe {
        let op = tuple::alloc_tuple_of(tp, visible);
        let n = visible + hidden_count(tp);
        let base = tuple::item_ptr(op);
        for i in 0..n {
            *base.add(i) = std::ptr::null_mut();
        }
        op
    }
}

/// Create an instance from exactly `n_fields` values (visible ones first).
pub fn new_instance(tp: *mut PyTypeObject, values: Vec<PyRef>) -> PyRef {
    let visible = (values.len()).saturating_sub(hidden_count(tp));
    unsafe {
        let op = alloc_instance(tp, visible);
        let base = tuple::item_ptr(op);
        for (i, v) in values.into_iter().enumerate() {
            *base.add(i) = v.into_raw();
        }
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}

unsafe extern "C" fn structseq_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let tp = (*op).type_ptr();
        let n = var_size(&*op) as usize + hidden_count(tp);
        let base = tuple::item_ptr(op);
        for i in 0..n {
            let p = std::mem::replace(&mut *base.add(i), std::ptr::null_mut());
            xdecref(p);
        }
        generic_free_and_type_decref(op, tp);
    }
}

unsafe extern "C" fn structseq_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let tp = (*op).type_ptr();
        if is_heap_type(tp) {
            let r = visit.unwrap()(tp as *mut PyObject, arg);
            if r != 0 {
                return r;
            }
        }
        let n = var_size(&*op) as usize + hidden_count(tp);
        let base = tuple::item_ptr(op);
        for i in 0..n {
            let it = *base.add(i);
            if !it.is_null() {
                let r = visit.unwrap()(it, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

/// Names of the named fields in member order (as CPython's tp_members).
fn member_names(tp: *mut PyTypeObject) -> Vec<String> {
    let mut v = Vec::new();
    unsafe {
        let mut m = (*tp).tp_members;
        if m.is_null() {
            return v;
        }
        while !(*m).name.is_null() {
            v.push(CStr::from_ptr((*m).name).to_string_lossy().into_owned());
            m = m.add(1);
        }
    }
    v
}

unsafe extern "C" fn structseq_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (vals, kws) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            static P: crate::args::Params = crate::args::Params::new("structseq", &["sequence", "dict"], 0, 1);
            let [seq, d] = crate::args::parse::<2>(&P, &refs, kws.as_deref())?;
            new_from_sequence(tp, seq.unwrap(), d)
        })();
        into_c(r)
    }
}

/// `tp(sequence, dict=None)` semantics.
pub fn new_from_sequence(tp: *mut PyTypeObject, seq: &PyObject, d: Option<&PyObject>) -> PyResult {
    let tpname = type_name(unsafe { &*tp });
    let full = unsafe { CStr::from_ptr((*tp).tp_name).to_string_lossy().into_owned() };
    let _ = tpname;
    let items: Vec<PyRef> = if tuple::is_tuple_exact(seq) {
        tuple::tuple_to_vec(seq)
    } else if crate::types::list::is_list_exact(seq) {
        crate::types::list::list_to_vec(seq)
    } else {
        if ab::get_iter(seq).is_err() {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                raise!(TypeError, "constructor requires a sequence");
            }
            return Err(PyErr);
        }
        ab::collect_iterable(seq)?
    };
    let d = match d {
        Some(x) if dict::is_dict(x) => Some(x),
        Some(_) => raise!(TypeError, "{}() takes a dict as second arg, if any", full),
        None => None,
    };
    let len = items.len();
    let min_len = n_sequence_fields(tp)?;
    let max_len = n_fields(tp)?;
    let n_unnamed = n_unnamed_fields(tp)?;
    if len < min_len {
        if min_len == max_len {
            raise!(TypeError, "{}() takes a {}-sequence ({}-sequence given)", full, min_len, len);
        }
        raise!(TypeError, "{}() takes an at least {}-sequence ({}-sequence given)", full, min_len, len);
    }
    if len > max_len {
        if min_len == max_len {
            raise!(TypeError, "{}() takes a {}-sequence ({}-sequence given)", full, min_len, len);
        }
        raise!(TypeError, "{}() takes an at most {}-sequence ({}-sequence given)", full, max_len, len);
    }
    let names = member_names(tp);
    let mut values = items;
    let mut count = 0usize;
    for i in len..max_len {
        let v = match (d, names.get(i - n_unnamed)) {
            (Some(d), Some(nm)) => match dict::get_item_rs(d, nm) {
                Some(v) => {
                    count += 1;
                    v
                }
                None => none::none_ref(),
            },
            _ => none::none_ref(),
        };
        values.push(v);
    }
    if let Some(d) = d {
        if dict::dict_len(d) > count {
            raise!(TypeError, "{}() got duplicate or unexpected field name(s)", full);
        }
    }
    unsafe {
        let op = alloc_instance(tp, min_len);
        let base = tuple::item_ptr(op);
        for (i, v) in values.into_iter().enumerate() {
            *base.add(i) = v.into_raw();
        }
        crate::gc::track(op);
        Ok(PyRef::from_raw(op))
    }
}

unsafe extern "C" fn structseq_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let r = (|| -> PyResult {
            let tp = o.type_ptr();
            let full = CStr::from_ptr((*tp).tp_name).to_string_lossy().into_owned();
            if crate::modules::reprlib_guard::enter(o) {
                return Ok(pystr::new_str(&format!("{}(...)", full)));
            }
            struct G<'a>(&'a PyObject);
            impl Drop for G<'_> {
                fn drop(&mut self) {
                    crate::modules::reprlib_guard::leave(self.0);
                }
            }
            let _g = G(o);
            let names = member_names(tp);
            let mut s = String::with_capacity(64);
            s.push_str(&full);
            s.push('(');
            let n = var_size(o) as usize;
            for i in 0..n {
                if i > 0 {
                    s.push_str(", ");
                }
                let nm = names.get(i).map(|x| x.as_str()).unwrap_or("?");
                s.push_str(nm);
                s.push('=');
                s.push_str(&ab::repr_string(get_item(o, i))?);
            }
            s.push(')');
            Ok(pystr::new_str(&s))
        })();
        into_c(r)
    }
}

fn structseq_reduce(o: &PyObject) -> PyResult {
    let tp = o.type_ptr();
    let n_visible = var_size(o) as usize;
    let n_fields = n_fields(tp)?;
    let n_unnamed = n_unnamed_fields(tp)?;
    let names = member_names(tp);
    let visible: Vec<PyRef> = (0..n_visible).map(|i| get_item(o, i).to_ref()).collect();
    let tup = tuple::new_tuple(visible);
    let d = dict::new_dict();
    for i in n_visible..n_fields.min(instance_slots(o)) {
        if let Some(nm) = names.get(i - n_unnamed) {
            dict::set_item_str(&d, nm, get_item(o, i))?;
        }
    }
    Ok(tuple::new_tuple(vec![type_ref(tp), tuple::new_tuple(vec![tup, d])]))
}

fn structseq_replace(o: &PyObject, args: &[&PyObject], kwnames: Option<&PyObject>) -> PyResult {
    let (pos, kws) = crate::args::split(args, kwnames);
    if !pos.is_empty() {
        raise!(TypeError, "__replace__() takes no positional arguments");
    }
    let tp = o.type_ptr();
    if n_unnamed_fields(tp)? > 0 {
        raise!(TypeError, "__replace__() is not supported");
    }
    let n_fields = n_fields(tp)?;
    let names = member_names(tp);
    let mut used = vec![false; kws.len()];
    let mut values = Vec::with_capacity(n_fields);
    for i in 0..n_fields {
        let nm = &names[i];
        let mut v = None;
        for (j, (k, val)) in kws.iter().enumerate() {
            if pystr::is_str(k) && pystr::to_string_lossy(k) == *nm {
                v = Some(val.to_ref());
                used[j] = true;
            }
        }
        values.push(v.unwrap_or_else(|| get_item(o, i).to_ref()));
    }
    let unexpected: Vec<String> =
        kws.iter().zip(used.iter()).filter(|(_, u)| !**u).map(|((k, _), _)| ab::repr_string(k).unwrap_or_default()).collect();
    if !unexpected.is_empty() {
        raise!(TypeError, "Got unexpected field name(s): {{{}}}", unexpected.join(", "));
    }
    Ok(new_instance(tp, values))
}

/// Create a new struct sequence type.
pub fn new_type(desc: &Desc) -> PyResult<*mut PyTypeObject> {
    let tp = Box::leak(Box::new(PyTypeObject::ZERO)) as *mut PyTypeObject;
    init_type(tp, desc)?;
    // module-level struct sequences (time.struct_time, os.stat_result, ...) are
    // created at run time like heap types: their attributes can be set
    unsafe { (*tp).tp_flags &= !Py_TPFLAGS_IMMUTABLETYPE };
    Ok(tp)
}

/// Initialise a (zeroed, possibly static) type object as a struct sequence type.
pub fn init_type(tp: *mut PyTypeObject, desc: &Desc) -> PyResult<()> {
    let n_fields = desc.fields.len();
    let n_visible = desc.n_in_sequence;
    let n_hidden = n_fields - n_visible;
    let n_unnamed = desc.fields.iter().filter(|f| f.is_unnamed()).count();
    unsafe {
        let t = &mut *tp;
        t.tp_name = crate::methods::cstr_static(desc.name);
        t.tp_doc = if desc.doc.is_empty() { std::ptr::null() } else { crate::methods::cstr_static(desc.doc) };
        t.tp_basicsize = (HEADER + n_hidden * PTR) as isize;
        t.tp_itemsize = PTR as isize;
        t.tp_base = tuple::tuple_type();
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_TUPLE_SUBCLASS;
        t.tp_dealloc = Some(structseq_dealloc);
        t.tp_traverse = Some(structseq_traverse);
        t.tp_repr = Some(structseq_repr);
        t.tp_new = Some(structseq_new);
        t.tp_free = Some(crate::types::typeobject::generic_free);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_methods = crate::mdefs![
            ("__reduce__", NOARGS, structseq_reduce),
            ("__replace__", FASTKW, structseq_replace, "__replace__($self, /, **changes)\n--\n\nReturn a copy with the named fields replaced."),
        ];
        let mut members = Vec::new();
        for (i, f) in desc.fields.iter().enumerate() {
            if f.is_unnamed() {
                continue;
            }
            members.push(crate::methods::mbdef(f.name, Py_T_OBJECT, HEADER + i * PTR, Py_READONLY, f.doc));
        }
        t.tp_members = crate::methods::mbtable(members);
        type_ready(tp)?;
        let d = type_dict(tp);
        dict::set_item_str(d, "n_sequence_fields", &int::new_int(n_visible as i64))?;
        dict::set_item_str(d, "n_fields", &int::new_int(n_fields as i64))?;
        dict::set_item_str(d, "n_unnamed_fields", &int::new_int(n_unnamed as i64))?;
        let ma: Vec<PyRef> =
            desc.fields[..n_visible].iter().filter(|f| !f.is_unnamed()).map(|f| pystr::intern_str(f.name)).collect();
        dict::set_item_str(d, "__match_args__", &tuple::new_tuple(ma))?;
    }
    Ok(())
}

/// Make instances of `tp` non-constructible from Python (like sys.flags).
pub fn disallow_instantiation(tp: *mut PyTypeObject) {
    unsafe {
        (*tp).tp_new = None;
        let d = type_dict(tp);
        let _ = dict::del_item(d, crate::name!("__new__"));
    }
}

// ---------------------------------------------------------------- C API

#[repr(C)]
pub struct PyStructSequence_Field {
    pub name: *const c_char,
    pub doc: *const c_char,
}
#[repr(C)]
pub struct PyStructSequence_Desc {
    pub name: *const c_char,
    pub doc: *const c_char,
    pub fields: *mut PyStructSequence_Field,
    pub n_in_sequence: c_int,
}

unsafe fn desc_from_c(desc: *mut PyStructSequence_Desc) -> (String, String, Vec<Field>, usize) {
    unsafe {
        let name = CStr::from_ptr((*desc).name).to_string_lossy().into_owned();
        let doc = if (*desc).doc.is_null() { String::new() } else { CStr::from_ptr((*desc).doc).to_string_lossy().into_owned() };
        let mut fields = Vec::new();
        let mut f = (*desc).fields;
        while !(*f).name.is_null() {
            let fname = if (*f).name == PyStructSequence_UnnamedField.as_ptr() {
                UNNAMED
            } else {
                Box::leak(CStr::from_ptr((*f).name).to_string_lossy().into_owned().into_boxed_str())
            };
            let fdoc: &'static str = if (*f).doc.is_null() {
                ""
            } else {
                Box::leak(CStr::from_ptr((*f).doc).to_string_lossy().into_owned().into_boxed_str())
            };
            fields.push(Field { name: fname, doc: fdoc });
            f = f.add(1);
        }
        (name, doc, fields, (*desc).n_in_sequence as usize)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyStructSequence_NewType(desc: *mut PyStructSequence_Desc) -> *mut PyTypeObject {
    unsafe {
        let (name, doc, fields, n) = desc_from_c(desc);
        match new_type(&Desc { name: &name, doc: &doc, fields: &fields, n_in_sequence: n }) {
            Ok(t) => t,
            Err(_) => std::ptr::null_mut(),
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyStructSequence_InitType2(tp: *mut PyTypeObject, desc: *mut PyStructSequence_Desc) -> c_int {
    unsafe {
        let (name, doc, fields, n) = desc_from_c(desc);
        int_into_c(init_type(tp, &Desc { name: &name, doc: &doc, fields: &fields, n_in_sequence: n }))
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyStructSequence_InitType(tp: *mut PyTypeObject, desc: *mut PyStructSequence_Desc) {
    unsafe {
        let _ = PyStructSequence_InitType2(tp, desc);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyStructSequence_New(tp: *mut PyTypeObject) -> *mut PyObject {
    unsafe {
        let visible = match n_sequence_fields(tp) {
            Ok(n) => n,
            Err(_) => return std::ptr::null_mut(),
        };
        let op = alloc_instance(tp, visible);
        crate::gc::track(op);
        op
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyStructSequence_SetItem(op: *mut PyObject, i: Py_ssize_t, v: *mut PyObject) {
    unsafe {
        let slot = tuple::item_ptr(op).add(i as usize);
        let old = std::mem::replace(&mut *slot, v);
        xdecref(old);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyStructSequence_GetItem(op: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe { *tuple::item_ptr(op).add(i as usize) }
}
