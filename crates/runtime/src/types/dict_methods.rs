//! dict type: slots, methods and type registration.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, dictview, exceptions, frozendict, int, none, str as pystr, tuple};
use std::ffi::c_int;

pub fn raise_key_error(k: &PyObject) {
    let _ = set_err_obj(exceptions::KeyError(), k.to_ref());
}

#[inline]
fn ni() -> PyRef {
    none::not_implemented().to_ref()
}

/// Is `o` a dict-layout object whose iteration order is the plain dict order
/// (no overridden `__iter__`)?  Then its entries may be read directly.
pub fn is_plain_iter_dict(o: &PyObject) -> bool {
    dict::is_anydict(o) && o.ty().tp_iter == Some(dict_iter)
}

/// d.update(o) for mappings (has keys()).
pub fn dict_update_from(d: &PyObject, o: &PyObject) -> PyResult<()> {
    if is_plain_iter_dict(o) {
        return dict::merge_dict(d, o, true);
    }
    let keys = ab::call_method_str(o, "keys", &[])?;
    let it = ab::get_iter(&keys)?;
    while let Some(k) = ab::iter_next(&it)? {
        let v = ab::getitem(o, &k)?;
        dict::set_item(d, &k, &v)?;
    }
    Ok(())
}

/// Update from an iterable of key/value pairs.
pub fn merge_from_seq2(d: &PyObject, src: &PyObject, overwrite: bool) -> PyResult<()> {
    let it = ab::get_iter(src)?;
    let mut i = 0usize;
    while let Some(item) = ab::iter_next(&it)? {
        let (k, v) = if tuple::is_tuple_exact(&item) && tuple::tuple_len(&item) == 2 {
            (tuple::tuple_get(&item, 0).to_ref(), tuple::tuple_get(&item, 1).to_ref())
        } else {
            let pair = match (|| {
                // PySequence_Fast(item, "object is not iterable") semantics
                let pit = match ab::get_iter(&item) {
                    Ok(p) => p,
                    Err(e) => {
                        if exc_matches(exceptions::TypeError()) {
                            clear_exc();
                            return Err(err!(TypeError, "object is not iterable"));
                        }
                        return Err(e);
                    }
                };
                let mut v = Vec::with_capacity(2);
                while let Some(x) = ab::iter_next(&pit)? {
                    v.push(x);
                }
                Ok::<_, PyErr>(v)
            })() {
                Ok(v) => v,
                Err(e) => {
                    if exc_matches(exceptions::TypeError()) {
                        crate::modules::traceback_util::add_note_to_current(&format!(
                            "Cannot convert dictionary update sequence element #{} to a sequence",
                            i
                        ));
                    }
                    return Err(e);
                }
            };
            if pair.len() != 2 {
                raise!(ValueError, "dictionary update sequence element #{} has length {}; 2 is required", i, pair.len());
            }
            let mut p = pair.into_iter();
            (p.next().unwrap(), p.next().unwrap())
        };
        if overwrite {
            dict::set_item(d, &k, &v)?;
        } else {
            let h = dict::hash_for(d, &k)?;
            if !dict::contains_hash(d, &k, h)? {
                dict::set_item_hash(d, &k, h, &v)?;
            }
        }
        i += 1;
    }
    Ok(())
}

/// dict_update_arg: mapping (has keys) or iterable of pairs.
pub fn dict_update_any(d: &PyObject, src: &PyObject) -> PyResult<()> {
    if dict::is_dict_exact(src) || is_plain_iter_dict(src) {
        return dict::merge_dict(d, src, true);
    }
    if ab::getattr_opt(src, crate::name!("keys"))?.is_some() {
        return dict_update_from(d, src);
    }
    merge_from_seq2(d, src, true)
}

/// Merge **kwargs mapping into d, raising on duplicates (f(**a, **b)).
pub fn dict_merge_kwargs(d: &PyObject, o: &PyObject, func: *mut PyObject) -> PyResult<()> {
    let fname = || -> String {
        if func.is_null() {
            return "function".into();
        }
        let f = unsafe { &*func };
        match ab::getattr_opt(f, crate::name!("__qualname__")) {
            Ok(Some(q)) if pystr::is_str(&q) => format!("{}()", pystr::to_string_lossy(&q)),
            _ => {
                clear_exc();
                format!("{} object", tname(f))
            }
        }
    };
    let pairs: Vec<(PyRef, PyRef)> = if is_plain_iter_dict(o) {
        dict::items_vec(o)
    } else {
        let keys = match ab::getattr_opt(o, crate::name!("keys"))? {
            Some(k) => ab::call(&k, &[])?,
            None => raise!(TypeError, "{} argument after ** must be a mapping, not {}", fname(), tname(o)),
        };
        let mut v = Vec::new();
        for k in ab::collect_iterable(&keys)? {
            let val = ab::getitem(o, &k)?;
            v.push((k, val));
        }
        v
    };
    for (k, v) in pairs {
        if !pystr::is_str(&k) {
            raise!(TypeError, "keywords must be strings");
        }
        if dict::contains(d, &k)? {
            raise!(TypeError, "{} got multiple values for keyword argument '{}'", fname(), pystr::to_string_lossy(&k));
        }
        dict::set_item(d, &k, &v)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ shared helpers (dict + frozendict)

/// `{k: v, ...}` body (with recursion guard); `None` if recursive.
pub fn repr_body(o: &PyObject) -> PyResult<Option<PyRef>> {
    if crate::modules::reprlib_guard::enter(o) {
        return Ok(None);
    }
    let mut sb = pystr::StrBuilder::new();
    sb.push('{' as u32);
    let res = (|| {
        let mut pos = 0;
        let mut first = true;
        while let Some((k, v)) = dict::next_entry(o, &mut pos) {
            if !first {
                sb.push_str(", ");
            }
            first = false;
            sb.push_obj(&*ab::repr(&k)?);
            sb.push_str(": ");
            sb.push_obj(&*ab::repr(&v)?);
        }
        Ok::<(), PyErr>(())
    })();
    crate::modules::reprlib_guard::leave(o);
    res?;
    sb.push('}' as u32);
    Ok(Some(sb.finish()))
}

/// Mapping equality for dict-layout objects.
pub fn dict_equal(a: &PyObject, b: &PyObject) -> PyResult<bool> {
    if dict::dict_len(a) != dict::dict_len(b) {
        return Ok(false);
    }
    let mut pos = 0;
    while let Some((k, v, h)) = dict::next_entry_hash(a, &mut pos) {
        match dict::get_item_hash(b, &k, h)? {
            None => return Ok(false),
            Some(bv) => {
                if !bv.is(&v) && !ab::rich_compare_bool(&v, &bv, Py_EQ)? {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

pub fn anydict_richcompare(a: &PyObject, b: &PyObject, op: c_int) -> PyResult {
    if !dict::is_anydict(a) || !dict::is_anydict(b) || (op != Py_EQ && op != Py_NE) {
        return Ok(ni());
    }
    let eq = dict_equal(a, b)?;
    Ok(bool_::new_bool(if op == Py_EQ { eq } else { !eq }))
}

/// Subscript with `__missing__` support for subclasses.
pub fn anydict_getitem(o: &PyObject, k: &PyObject, exact: bool) -> PyResult {
    let h = dict::hash_for(o, k)?;
    match dict::get_item_hash(o, k, h)? {
        Some(v) => Ok(v),
        None => {
            if !exact {
                if let Some(m) = ab::lookup_special(o, crate::name!("__missing__"))? {
                    return ab::call(&m, &[k]);
                }
            }
            raise_key_error(k);
            Err(PyErr)
        }
    }
}

pub fn get_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get", args, 1, 2)?;
    match dict::get_item(o, args[0])? {
        Some(v) => Ok(v),
        None => Ok(args.get(1).map(|x| x.to_ref()).unwrap_or_else(none::none_ref)),
    }
}

pub fn sizeof_m(o: &PyObject) -> PyResult {
    let base = o.ty().tp_basicsize as usize;
    Ok(int::new_int((base + dict::table_sizeof(o)) as i64))
}

pub fn keys_m(o: &PyObject) -> PyResult {
    dictview::new_view(o, dictview::Kind::Keys)
}
pub fn values_m(o: &PyObject) -> PyResult {
    dictview::new_view(o, dictview::Kind::Values)
}
pub fn items_m(o: &PyObject) -> PyResult {
    dictview::new_view(o, dictview::Kind::Items)
}
pub fn reversed_m(o: &PyObject) -> PyResult {
    dictview::new_dict_iter(o, dictview::Kind::Keys, true)
}
pub fn class_getitem_m(cls: &PyObject, item: &PyObject) -> PyResult {
    crate::types::containers_reg::generic_alias(cls, item)
}

/// fromkeys shared by dict and frozendict.
pub fn fromkeys_impl(cls: &PyObject, iterable: &PyObject, value: &PyObject) -> PyResult {
    let d = ab::call(cls, &[])?;
    if frozendict::is_frozendict(&d) {
        // Build in a temporary dict, then construct through the class.
        let tmp = dict::copy(&d)?;
        fill_fromkeys(&tmp, iterable, value)?;
        return ab::call(cls, &[&tmp]);
    }
    fill_fromkeys(&d, iterable, value)?;
    Ok(d)
}

fn fill_fromkeys(d: &PyObject, iterable: &PyObject, value: &PyObject) -> PyResult<()> {
    if dict::is_dict_exact(d) {
        if dict::is_anydict(iterable) && is_plain_iter_dict(iterable) {
            for (k, _, h) in dict::items_hash_vec(iterable) {
                dict::set_item_hash(d, &k, h, value)?;
            }
            return Ok(());
        }
        if crate::types::set::is_anyset(iterable) {
            for (k, h) in crate::types::set::items_with_hash(iterable) {
                dict::set_item_hash(d, &k, h, value)?;
            }
            return Ok(());
        }
        let it = ab::get_iter(iterable)?;
        while let Some(k) = ab::iter_next(&it)? {
            dict::set_item(d, &k, value)?;
        }
        return Ok(());
    }
    let it = ab::get_iter(iterable)?;
    while let Some(k) = ab::iter_next(&it)? {
        ab::setitem(d, &k, Some(value))?;
    }
    Ok(())
}

// ------------------------------------------------------------------ dict slots

unsafe extern "C" fn dict_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        if dict::dict_len(o) == 0 {
            return pystr::new_str("{}").into_raw();
        }
        into_c(repr_body(o).map(|r| r.unwrap_or_else(|| pystr::new_str("{...}"))))
    }
}
unsafe extern "C" fn dict_len_slot(o: *mut PyObject) -> Py_ssize_t {
    unsafe { dict::dict_len(&*o) as isize }
}
unsafe extern "C" fn dict_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        into_c(anydict_getitem(o, &*k, dict::is_dict_exact(o)))
    }
}
unsafe extern "C" fn dict_ass_subscript(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let (o, k) = (&*o, &*k);
        if v.is_null() {
            return match dict::del_item(o, k) {
                Ok(true) => 0,
                Ok(false) => {
                    raise_key_error(k);
                    -1
                }
                Err(_) => -1,
            };
        }
        int_into_c(dict::set_item(o, k, &*v))
    }
}
pub unsafe extern "C" fn dict_contains(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        match dict::contains(&*o, &*k) {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}
pub unsafe extern "C" fn dict_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(dictview::new_dict_iter(&*o, dictview::Kind::Keys, false)) }
}
unsafe extern "C" fn dict_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe { into_c(anydict_richcompare(&*a, &*b, op)) }
}
unsafe extern "C" fn dict_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if !op.is_null() {
            dict::init_dict_storage(op);
        }
        op
    }
}
unsafe extern "C" fn dict_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "dict expected at most 1 argument, got {}", a.len());
            }
            if let Some(src) = a.first() {
                dict_update_any(&*o, src)?;
            }
            if !kw.is_null() && dict::dict_len(&*kw) > 0 {
                dict::merge_dict(&*o, &*kw, true)?;
            }
            Ok(())
        })();
        int_into_c(r)
    }
}
unsafe extern "C" fn dict_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        let r = (|| -> PyResult {
            if !dict::is_dict(a) || !dict::is_anydict(b) {
                return Ok(ni());
            }
            let n = dict::copy(a)?;
            dict_update_any(&n, b)?;
            Ok(n)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn dict_ior(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = &*a;
        into_c(dict_update_any(a, &*b).map(|_| a.to_ref()))
    }
}

// ------------------------------------------------------------------ dict methods

fn setdefault_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setdefault", args, 1, 2)?;
    let d = args.get(1).copied().unwrap_or(none::none());
    let h = dict::hash_for(o, args[0])?;
    dict::setdefault_hash(o, args[0], h, d)
}
fn pop_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pop", args, 1, 2)?;
    let found = if dict::dict_len(o) == 0 { None } else { dict::pop_item(o, args[0])? };
    match found {
        Some(v) => Ok(v),
        None => match args.get(1) {
            Some(d) => Ok(d.to_ref()),
            None => {
                raise_key_error(args[0]);
                Err(PyErr)
            }
        },
    }
}
fn popitem_m(o: &PyObject) -> PyResult {
    match dict::pop_last(o) {
        Some((k, v)) => Ok(tuple::new_tuple(vec![k, v])),
        None => raise!(KeyError, "popitem(): dictionary is empty"),
    }
}
fn update_m(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = crate::args::split(args, kw);
    if pos.len() > 1 {
        raise!(TypeError, "update expected at most 1 argument, got {}", pos.len());
    }
    if let Some(src) = pos.first() {
        dict_update_any(o, src)?;
    }
    for (k, v) in kws {
        dict::set_item(o, k, v)?;
    }
    Ok(none::none_ref())
}
fn clear_m(o: &PyObject) -> PyResult {
    dict::clear(o);
    Ok(none::none_ref())
}
fn copy_m(o: &PyObject) -> PyResult {
    dict::copy(o)
}
fn fromkeys_m(cls: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("fromkeys", args, 1, 2)?;
    fromkeys_impl(cls, args[0], args.get(1).copied().unwrap_or(none::none()))
}

static mut DICT_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut DICT_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut DICT_NUM: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    unsafe {
        let t = &mut *dict::dict_type();
        t.tp_name = c"dict".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyDictObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_DICT_SUBCLASS | Py_TPFLAGS_MAPPING;
        t.tp_dealloc = Some(dict::dict_dealloc);
        t.tp_traverse = Some(dict::dict_traverse);
        t.tp_clear = Some(dict::dict_clear_slot);
        t.tp_repr = Some(dict_repr);
        t.tp_hash = Some(crate::types::slots::hash_not_implemented);
        t.tp_richcompare = Some(dict_richcompare);
        t.tp_iter = Some(dict_iter);
        t.tp_new = Some(dict_new);
        t.tp_init = Some(dict_init);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"dict() -> new empty dictionary\ndict(mapping) -> new dictionary initialized from a mapping object's\n    (key, value) pairs\ndict(iterable) -> new dictionary initialized as if via:\n    d = {}\n    for k, v in iterable:\n        d[k] = v\ndict(**kwargs) -> new dictionary initialized with the name=value pairs\n    in the keyword argument list.  For example:  dict(one=1, two=2)".as_ptr();
        let s = &mut *(&raw mut DICT_SEQ);
        s.sq_contains = Some(dict_contains);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut DICT_MAP);
        m.mp_length = Some(dict_len_slot);
        m.mp_subscript = Some(dict_subscript);
        m.mp_ass_subscript = Some(dict_ass_subscript);
        t.tp_as_mapping = m;
        let n = &mut *(&raw mut DICT_NUM);
        n.nb_or = Some(dict_or);
        n.nb_inplace_or = Some(dict_ior);
        t.tp_as_number = n;
        t.tp_methods = crate::mdefs![
            ("__sizeof__", NOARGS, sizeof_m, "__sizeof__($self, /)\n--\n\nReturn the size of the dict in memory, in bytes."),
            ("__reversed__", NOARGS, reversed_m, "__reversed__($self, /)\n--\n\nReturn a reverse iterator over the dict keys."),
            ("__class_getitem__", O, class_getitem_m, "See PEP 585"; METH_CLASS),
            ("get", FAST, get_m, "get($self, key, default=None, /)\n--\n\nReturn the value for key if key is in the dictionary, else default."),
            ("setdefault", FAST, setdefault_m, "setdefault($self, key, default=None, /)\n--\n\nInsert key with a value of default if key is not in the dictionary.\n\nReturn the value for key if key is in the dictionary, else default."),
            ("pop", FAST, pop_m, "pop($self, key, default=<unrepresentable>, /)\n--\n\nD.pop(k[,d]) -> v, remove specified key and return the corresponding value.\n\nIf the key is not found, return the default if given; otherwise,\nraise a KeyError."),
            ("popitem", NOARGS, popitem_m, "popitem($self, /)\n--\n\nRemove and return a (key, value) pair as a 2-tuple.\n\nPairs are returned in LIFO (last-in, first-out) order.\nRaises KeyError if the dict is empty."),
            ("keys", NOARGS, keys_m, "keys($self, /)\n--\n\nReturn a set-like object providing a view on the dict's keys."),
            ("values", NOARGS, values_m, "values($self, /)\n--\n\nReturn an object providing a view on the dict's values."),
            ("items", NOARGS, items_m, "items($self, /)\n--\n\nReturn a set-like object providing a view on the dict's items."),
            ("update", FASTKW, update_m, "D.update([E, ]**F) -> None.  Update D from mapping/iterable E and F.\nIf E is present and has a .keys() method, then does:  for k in E.keys(): D[k] = E[k]\nIf E is present and lacks a .keys() method, then does:  for k, v in E: D[k] = v\nIn either case, this is followed by: for k in F:  D[k] = F[k]"),
            ("clear", NOARGS, clear_m, "clear($self, /)\n--\n\nRemove all items from the dict."),
            ("copy", NOARGS, copy_m, "copy($self, /)\n--\n\nReturn a shallow copy of the dict."),
            ("fromkeys", FAST, fromkeys_m, "fromkeys($type, iterable, value=None, /)\n--\n\nCreate a new dictionary with keys from iterable and values set to value."; METH_CLASS),
        ];
    }
}
