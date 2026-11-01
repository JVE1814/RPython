//! Struct sequences (named tuples implemented in Rust): `sys.flags`, `sys.float_info`,
//! `_thread._ExceptHookArgs`, ... Layout: a tuple subclass whose `ob_item` holds all
//! `n_fields` values, while `ob_size` is `n_sequence_fields` (the visible part).
//! (System workstream copy; to be unified with types/structseq.rs when that lands.)

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::{type_name, type_ref};
use crate::types::{dict, int, none, str as pystr, tuple};
use crate::raise;
use std::ffi::{c_int, c_void};

/// Name used for unnamed (positional-only) fields.
pub const UNNAMED: &str = "unnamed field";

#[derive(Clone)]
pub struct Desc {
    pub name: &'static str,
    pub doc: &'static str,
    pub fields: Vec<&'static str>,
    pub n_in_sequence: usize,
    /// Allow `T(seq, dict=None)` construction.
    pub allow_new: bool,
}

fn n_fields(tp: *mut PyTypeObject) -> usize {
    get_class_int(tp, "n_fields")
}
fn n_seq(tp: *mut PyTypeObject) -> usize {
    get_class_int(tp, "n_sequence_fields")
}
fn n_unnamed(tp: *mut PyTypeObject) -> usize {
    get_class_int(tp, "n_unnamed_fields")
}
fn get_class_int(tp: *mut PyTypeObject, nm: &str) -> usize {
    crate::types::typeobject::lookup_str(tp, nm).and_then(|v| int::as_i64(&v)).unwrap_or(0) as usize
}

unsafe fn items(op: *mut PyObject) -> *mut *mut PyObject {
    unsafe { tuple::item_ptr(op) }
}

unsafe extern "C" fn ss_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let tp = (*op).type_ptr();
        let n = n_fields(tp).max(var_size(&*op) as usize);
        let base = items(op);
        for i in 0..n {
            xdecref(*base.add(i));
        }
        crate::types::typeobject::generic_free_and_type_decref(op, tp);
    }
}

unsafe extern "C" fn ss_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let tp = (*op).type_ptr();
        let n = n_fields(tp).max(var_size(&*op) as usize);
        let base = items(op);
        for i in 0..n {
            let p = *base.add(i);
            if !p.is_null() {
                let r = visit.unwrap()(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

/// Create an instance with all `n_fields` values (missing trailing values become None).
pub fn new_instance(tp: *mut PyTypeObject, values: Vec<PyRef>) -> PyRef {
    unsafe {
        let nf = n_fields(tp);
        let ns = n_seq(tp);
        let op = tuple::alloc_tuple_of(tp, nf.max(values.len()));
        let base = items(op);
        let mut i = 0;
        for v in values {
            *base.add(i) = v.into_raw();
            i += 1;
        }
        while i < nf {
            *base.add(i) = none::none_ref().into_raw();
            i += 1;
        }
        set_var_size(op, ns as isize);
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}

/// Get field `i` (any index < n_fields).
pub fn get_field(o: &PyObject, i: usize) -> PyRef {
    unsafe { PyRef::from_borrowed(*items(o.as_ptr()).add(i)) }
}

unsafe extern "C" fn ss_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let o = &*op;
            let tp = o.type_ptr();
            let fields = field_names(tp);
            let ns = var_size(o) as usize;
            let mut s = String::new();
            s.push_str(&type_name_full(tp));
            s.push('(');
            if crate::modules::reprlib_guard::enter(o) {
                s.push_str("...)");
                return Ok(pystr::new_str(&s));
            }
            let res = (|| -> PyResult<()> {
                for i in 0..ns {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let nm = fields.get(i).map(|x| x.as_str()).unwrap_or(UNNAMED);
                    s.push_str(nm);
                    s.push('=');
                    s.push_str(&ab::repr_string(&get_field(o, i))?);
                }
                Ok(())
            })();
            crate::modules::reprlib_guard::leave(o);
            res?;
            s.push(')');
            Ok(pystr::new_str(&s))
        })();
        into_c(r)
    }
}

fn type_name_full(tp: *mut PyTypeObject) -> String {
    unsafe { std::ffi::CStr::from_ptr((*tp).tp_name).to_string_lossy().into_owned() }
}

fn field_names(tp: *mut PyTypeObject) -> Vec<String> {
    // stored as a tuple in the private class attribute `_ss_fields`
    match crate::types::typeobject::lookup_str(tp, "_ss_fields") {
        Some(t) if tuple::is_tuple(&t) => tuple::tuple_items(&t).iter().map(|x| pystr::to_string_lossy(x)).collect(),
        _ => Vec::new(),
    }
}

fn ss_reduce(o: &PyObject) -> PyResult {
    let tp = o.type_ptr();
    let ns = var_size(o) as usize;
    let nf = n_fields(tp);
    let nu = n_unnamed(tp);
    let fields = field_names(tp);
    let vis: Vec<PyRef> = (0..ns).map(|i| get_field(o, i)).collect();
    let d = dict::new_dict();
    for i in ns..nf {
        let nm = &fields[i];
        if nm != UNNAMED {
            dict::set_item_str(&d, nm, &get_field(o, i))?;
        }
    }
    let _ = nu;
    let args = tuple::new_tuple(vec![tuple::new_tuple(vis), d]);
    Ok(tuple::new_tuple(vec![type_ref(tp), args]))
}

fn ss_replace(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = crate::args::split(args, kw);
    if !pos.is_empty() {
        raise!(TypeError, "__replace__() takes no positional arguments");
    }
    let tp = o.type_ptr();
    if n_unnamed(tp) > 0 {
        raise!(TypeError, "__replace__() is not supported");
    }
    let fields = field_names(tp);
    let nf = n_fields(tp);
    let mut vals: Vec<PyRef> = (0..nf).map(|i| get_field(o, i)).collect();
    let mut used = 0;
    for (k, v) in &kws {
        let ks = pystr::to_string_lossy(k);
        match fields.iter().position(|f| *f == ks) {
            Some(i) => {
                vals[i] = v.to_ref();
                used += 1;
            }
            None => {
                let mut bad: Vec<String> = kws
                    .iter()
                    .map(|(k, _)| pystr::to_string_lossy(k))
                    .filter(|k| !fields.contains(k))
                    .map(|k| format!("'{}'", k))
                    .collect();
                bad.dedup();
                raise!(TypeError, "{}.__replace__() got unexpected keyword argument(s): {}", short_name(tp), bad.join(", "));
            }
        }
    }
    let _ = used;
    Ok(new_instance(tp, vals))
}

fn short_name(tp: *mut PyTypeObject) -> String {
    let n = type_name_full(tp);
    n.rsplit('.').next().unwrap_or(&n).to_string()
}

unsafe extern "C" fn ss_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let a = tuple::tuple_items(&*args);
            let mut seq: Option<PyRef> = a.first().map(|x| x.to_ref());
            let mut dct: Option<PyRef> = a.get(1).map(|x| x.to_ref());
            let name = short_name(tp);
            if a.len() > 2 {
                raise!(TypeError, "{}() takes at most 2 arguments ({} given)", type_name_full(tp), a.len());
            }
            if !kw.is_null() {
                for (k, v) in dict::items_vec(&*kw) {
                    let ks = pystr::to_string_lossy(&k);
                    if ks == "sequence" && seq.is_none() {
                        seq = Some(v);
                    } else if ks == "dict" && dct.is_none() {
                        dct = Some(v);
                    } else {
                        raise!(TypeError, "{}() got an unexpected keyword argument '{}'", type_name_full(tp), ks);
                    }
                }
            }
            let seq = match seq {
                Some(s) => s,
                None => raise!(TypeError, "{}() missing required argument 'sequence' (pos 1)", type_name_full(tp)),
            };
            let _ = name;
            let vals = ab::collect_iterable(&seq).map_err(|e| {
                if exc_matches(crate::types::exceptions::TypeError()) {
                    clear_exc();
                    err!(TypeError, "constructor requires a sequence")
                } else {
                    e
                }
            })?;
            let dct = dct.filter(|d| !d.is(none::none()));
            if let Some(d) = &dct {
                if !dict::is_dict(d) {
                    raise!(TypeError, "{}() takes a dict as second arg, if any", type_name_full(tp));
                }
            }
            let min_len = n_seq(tp);
            let max_len = n_fields(tp);
            let len = vals.len();
            let tn = type_name_full(tp);
            if len < min_len {
                if min_len == max_len {
                    raise!(TypeError, "{}() takes a {}-sequence ({}-sequence given)", tn, min_len, len);
                }
                raise!(TypeError, "{}() takes an at least {}-sequence ({}-sequence given)", tn, min_len, len);
            }
            if len > max_len {
                if min_len == max_len {
                    raise!(TypeError, "{}() takes a {}-sequence ({}-sequence given)", tn, min_len, len);
                }
                raise!(TypeError, "{}() takes an at most {}-sequence ({}-sequence given)", tn, max_len, len);
            }
            let fields = field_names(tp);
            let mut out = vals;
            let mut count = 0;
            for i in len..max_len {
                let nm = &fields[i];
                let v = match &dct {
                    Some(d) if nm != UNNAMED => match dict::get_item_rs(d, nm) {
                        Some(v) => {
                            count += 1;
                            v
                        }
                        None => none::none_ref(),
                    },
                    _ => none::none_ref(),
                };
                out.push(v);
            }
            if let Some(d) = &dct {
                if dict::dict_len(d) > count {
                    raise!(TypeError, "{}() got duplicate or unexpected field name(s)", tn);
                }
            }
            Ok(new_instance(tp, out))
        })();
        into_c(r)
    }
}

/// Create a new struct sequence type.
pub fn new_type(desc: &Desc) -> PyResult<*mut PyTypeObject> {
    unsafe {
        let tp: *mut PyTypeObject = Box::leak(Box::new(PyTypeObject::ZERO));
        let t = &mut *tp;
        let tuple_tp = tuple::tuple_type();
        t.tp_name = crate::methods::cstr_static(desc.name);
        t.tp_basicsize = (*tuple_tp).tp_basicsize;
        t.tp_itemsize = std::mem::size_of::<*mut PyObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_TUPLE_SUBCLASS | Py_TPFLAGS_SEQUENCE;
        t.tp_base = tuple_tp;
        t.tp_dealloc = Some(ss_dealloc);
        t.tp_traverse = Some(ss_traverse);
        t.tp_repr = Some(ss_repr);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        if !desc.doc.is_empty() {
            t.tp_doc = crate::methods::cstr_static(desc.doc);
        }
        t.tp_methods = crate::mdefs![("__reduce__", NOARGS, ss_reduce), ("__replace__", FASTKW, ss_replace),];
        let off0 = std::mem::offset_of!(PyTupleObject, ob_item);
        let mut members: Vec<PyMemberDef> = Vec::new();
        let mut nunnamed = 0;
        for (i, f) in desc.fields.iter().enumerate() {
            if *f == UNNAMED {
                nunnamed += 1;
                continue;
            }
            members.push(crate::methods::mbdef(f, Py_T_OBJECT, off0 + i * std::mem::size_of::<*mut PyObject>(), Py_READONLY, ""));
        }
        t.tp_members = crate::methods::mbtable(members);
        if desc.allow_new {
            t.tp_new = Some(ss_new);
        }
        crate::types::typeobject::type_ready(tp)?;
        if !desc.allow_new {
            t.tp_new = None;
            t.tp_flags |= Py_TPFLAGS_DISALLOW_INSTANTIATION;
        }
        let d = crate::types::typeobject::type_dict(tp);
        dict::set_item_str(d, "n_sequence_fields", &int::new_int(desc.n_in_sequence as i64))?;
        dict::set_item_str(d, "n_fields", &int::new_int(desc.fields.len() as i64))?;
        dict::set_item_str(d, "n_unnamed_fields", &int::new_int(nunnamed))?;
        let ma: Vec<PyRef> =
            desc.fields.iter().take(desc.n_in_sequence).filter(|f| **f != UNNAMED).map(|f| pystr::new_str(f)).collect();
        dict::set_item_str(d, "__match_args__", &tuple::new_tuple(ma))?;
        let all: Vec<PyRef> = desc.fields.iter().map(|f| pystr::new_str(f)).collect();
        dict::set_item_str(d, "_ss_fields", &tuple::new_tuple(all))?;
        if let Some(i) = desc.name.rfind('.') {
            dict::set_item_str(d, "__module__", &pystr::new_str(&desc.name[..i]))?;
        } else {
            dict::set_item_str(d, "__module__", &pystr::new_str("builtins"))?;
        }
        let _ = type_name(&*tp);
        Ok(tp)
    }
}

/// Convenience: create a type from a static field list.
pub fn make_type(name: &'static str, fields: &[&'static str], n_in_sequence: usize, allow_new: bool) -> PyResult<*mut PyTypeObject> {
    new_type(&Desc { name, doc: "", fields: fields.to_vec(), n_in_sequence, allow_new })
}
