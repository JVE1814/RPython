//! dict_keys / dict_values / dict_items views and the dict iterator types
//! (forward and reversed, keys/values/items).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, int, list, none, set, str as pystr, tuple};
use std::ffi::{c_int, c_void};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Keys,
    Values,
    Items,
}

// ================================================================== views

#[repr(C)]
pub struct DictView {
    ob_base: PyObject,
    dict: *mut PyObject,
}
#[unsafe(no_mangle)]
pub static mut PyDictKeys_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyDictValues_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyDictItems_Type: PyTypeObject = PyTypeObject::ZERO;

fn view_tp(k: Kind) -> *mut PyTypeObject {
    match k {
        Kind::Keys => &raw mut PyDictKeys_Type,
        Kind::Values => &raw mut PyDictValues_Type,
        Kind::Items => &raw mut PyDictItems_Type,
    }
}
// The view types have native subtypes (collections.OrderedDict's odict_keys,
// ...), which iterate in their own order; hence the subtype checks (grind-lib).
fn of_type(o: &PyObject, t: *mut PyTypeObject) -> bool {
    o.is_type(t) || is_subtype(o.type_ptr(), t)
}
fn kind_of(o: &PyObject) -> Kind {
    if of_type(o, &raw mut PyDictKeys_Type) {
        Kind::Keys
    } else if of_type(o, &raw mut PyDictValues_Type) {
        Kind::Values
    } else {
        Kind::Items
    }
}
pub fn is_view(o: &PyObject) -> bool {
    of_type(o, &raw mut PyDictKeys_Type) || of_type(o, &raw mut PyDictValues_Type) || of_type(o, &raw mut PyDictItems_Type)
}
/// keys or items view (set-like).
pub fn is_setlike_view(o: &PyObject) -> bool {
    of_type(o, &raw mut PyDictKeys_Type) || of_type(o, &raw mut PyDictItems_Type)
}
fn is_view_exact(o: &PyObject) -> bool {
    o.is_type(&raw mut PyDictKeys_Type) || o.is_type(&raw mut PyDictValues_Type) || o.is_type(&raw mut PyDictItems_Type)
}
fn subtype_view_items(o: &PyObject) -> PyResult<Vec<PyRef>> {
    let it = ab::get_iter(o)?;
    let mut v = Vec::new();
    while let Some(x) = ab::iter_next(&it)? {
        v.push(x);
    }
    Ok(v)
}
/// Allocate a view of `dct` with the given (sub)type of a dict view type.
pub fn new_view_of_type(dct: &PyObject, tp: *mut PyTypeObject) -> PyResult {
    unsafe {
        let op = crate::alloc::alloc_object(tp, std::mem::size_of::<DictView>());
        (*(op as *mut DictView)).dict = dct.to_ref().into_raw();
        crate::gc::track(op);
        Ok(PyRef::from_raw(op))
    }
}
fn vd(o: &PyObject) -> &PyObject {
    unsafe { &*(*(o.as_ptr() as *mut DictView)).dict }
}
pub fn view_dict(o: &PyObject) -> &PyObject {
    vd(o)
}
pub fn new_view(dct: &PyObject, k: Kind) -> PyResult {
    unsafe {
        let op = crate::alloc::alloc_object(view_tp(k), std::mem::size_of::<DictView>());
        (*(op as *mut DictView)).dict = dct.to_ref().into_raw();
        crate::gc::track(op);
        Ok(PyRef::from_raw(op))
    }
}
/// Snapshot of the view's elements.
pub fn view_items(o: &PyObject) -> Vec<PyRef> {
    let dd = vd(o);
    if !is_view_exact(o) {
        // a subtype view defines its own iteration order
        return subtype_view_items(o).unwrap_or_else(|_| {
            clear_exc();
            Vec::new()
        });
    }
    match kind_of(o) {
        Kind::Keys => dict::keys_vec(dd),
        Kind::Values => dict::values_vec(dd),
        Kind::Items => dict::items_vec(dd).into_iter().map(|(k, v)| tuple::tuple2(k, v)).collect(),
    }
}
unsafe extern "C" fn view_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut DictView)).dict);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn view_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let d = (*(o as *mut DictView)).dict;
        if d.is_null() { 0 } else { visit.unwrap()(d, arg) }
    }
}
unsafe extern "C" fn view_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe { dict::dict_len(vd(&*o)) as isize }
}
unsafe extern "C" fn view_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        into_c(new_dict_iter(vd(o), kind_of(o), false))
    }
}
fn items_contains(d: &PyObject, k: &PyObject) -> PyResult<bool> {
    if !tuple::is_tuple(k) || tuple::tuple_len(k) != 2 {
        return Ok(false);
    }
    match dict::get_item(d, tuple::tuple_get(k, 0))? {
        Some(v) => ab::rich_compare_bool(&v, tuple::tuple_get(k, 1), Py_EQ),
        None => Ok(false),
    }
}
pub fn view_contains_rs(o: &PyObject, k: &PyObject) -> PyResult<bool> {
    match kind_of(o) {
        Kind::Keys => dict::contains(vd(o), k),
        Kind::Items => items_contains(vd(o), k),
        Kind::Values => {
            let d = vd(o);
            let mut pos = 0;
            while let Some((_, v)) = dict::next_entry(d, &mut pos) {
                if ab::rich_compare_bool(&v, k, Py_EQ)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}
unsafe extern "C" fn view_contains(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        match view_contains_rs(&*o, &*k) {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn view_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let r = (|| -> PyResult {
            if crate::modules::reprlib_guard::enter(o) {
                return Ok(pystr::new_str("..."));
            }
            let res = (|| {
                let mut sb = pystr::StrBuilder::new();
                sb.push_str(&tname(o));
                sb.push_str("([");
                let items = if is_view_exact(o) { view_items(o) } else { subtype_view_items(o)? };
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        sb.push_str(", ");
                    }
                    sb.push_obj(&*ab::repr(it)?);
                }
                sb.push_str("])");
                Ok(sb.finish())
            })();
            crate::modules::reprlib_guard::leave(o);
            res
        })();
        into_c(r)
    }
}

/// Are all elements of `a` contained in `b`?
fn all_contained_in(a: &PyObject, b: &PyObject) -> PyResult<bool> {
    let it = ab::get_iter(a)?;
    while let Some(x) = ab::iter_next(&it)? {
        if !ab::contains(b, &x)? {
            return Ok(false);
        }
    }
    Ok(true)
}

unsafe extern "C" fn view_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        let r = (|| -> PyResult {
            if !set::is_anyset(b) && !is_setlike_view(b) {
                return Ok(none::not_implemented().to_ref());
            }
            let la = ab::length(a)?;
            let lb = ab::length(b)?;
            let ok = match op {
                Py_EQ | Py_NE => la == lb && all_contained_in(a, b)?,
                Py_LT => la < lb && all_contained_in(a, b)?,
                Py_LE => la <= lb && all_contained_in(a, b)?,
                Py_GT => la > lb && all_contained_in(b, a)?,
                Py_GE => la >= lb && all_contained_in(b, a)?,
                _ => return Ok(none::not_implemented().to_ref()),
            };
            Ok(bool_::new_bool(if op == Py_NE { !ok } else { ok }))
        })();
        into_c(r)
    }
}

fn mapping(o: &PyObject) -> PyResult {
    crate::types::mappingproxy::new_mappingproxy(vd(o))
}

fn view_reversed(o: &PyObject) -> PyResult {
    new_dict_iter(vd(o), kind_of(o), true)
}

// ---- set operations on keys/items views

fn binop_operands<'a>(a: &'a PyObject, b: &'a PyObject) -> Option<(&'a PyObject, &'a PyObject)> {
    if is_setlike_view(a) || is_setlike_view(b) { Some((a, b)) } else { None }
}

fn view_sub(a: &PyObject, b: &PyObject) -> PyResult {
    if binop_operands(a, b).is_none() {
        return Ok(none::not_implemented().to_ref());
    }
    let r = set::make_set(a)?;
    set::difference_update_with(&r, b)?;
    Ok(r)
}
fn view_and(a: &PyObject, b: &PyObject) -> PyResult {
    if binop_operands(a, b).is_none() {
        return Ok(none::not_implemented().to_ref());
    }
    // the view is the container probed; the other operand is iterated
    let (view, other) = if is_setlike_view(a) { (a, b) } else { (b, a) };
    let (iter_over, probe): (&PyObject, &PyObject) =
        if (set::is_anyset(other) || is_setlike_view(other)) && ab::length(view)? <= ab::length(other)? { (view, other) } else { (other, view) };
    let result = set::make_set_empty()?;
    let it = ab::get_iter(iter_over)?;
    while let Some(x) = ab::iter_next(&it)? {
        if ab::contains(probe, &x)? {
            set::set_add(&result, &x)?;
        }
    }
    Ok(result)
}
fn view_or(a: &PyObject, b: &PyObject) -> PyResult {
    if binop_operands(a, b).is_none() {
        return Ok(none::not_implemented().to_ref());
    }
    let r = set::make_set(a)?;
    set::set_update(&r, b)?;
    Ok(r)
}
fn view_xor(a: &PyObject, b: &PyObject) -> PyResult {
    if binop_operands(a, b).is_none() {
        return Ok(none::not_implemented().to_ref());
    }
    let r = set::make_set(a)?;
    set::symmetric_difference_update_with(&r, b)?;
    Ok(r)
}
fn view_isdisjoint(a: &PyObject, other: &PyObject) -> PyResult {
    let (small, big): (&PyObject, &PyObject) = if (set::is_anyset(other) || is_setlike_view(other)) && ab::length(other)? > ab::length(a)? {
        (a, other)
    } else {
        (other, a)
    };
    let it = ab::get_iter(small)?;
    while let Some(x) = ab::iter_next(&it)? {
        if ab::contains(big, &x)? {
            return Ok(bool_::new_bool(false));
        }
    }
    Ok(bool_::new_bool(true))
}

// ================================================================== iterators

#[repr(C)]
pub struct DictIter {
    ob_base: PyObject,
    dict: *mut PyObject,
    used: isize,
    pos: isize,
    len: isize,
}

#[unsafe(no_mangle)]
pub static mut PyDictIterKey_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyDictIterValue_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyDictIterItem_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyDictRevIterKey_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyDictRevIterValue_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyDictRevIterItem_Type: PyTypeObject = PyTypeObject::ZERO;

fn iter_tp(k: Kind, rev: bool) -> *mut PyTypeObject {
    match (k, rev) {
        (Kind::Keys, false) => &raw mut PyDictIterKey_Type,
        (Kind::Values, false) => &raw mut PyDictIterValue_Type,
        (Kind::Items, false) => &raw mut PyDictIterItem_Type,
        (Kind::Keys, true) => &raw mut PyDictRevIterKey_Type,
        (Kind::Values, true) => &raw mut PyDictRevIterValue_Type,
        (Kind::Items, true) => &raw mut PyDictRevIterItem_Type,
    }
}
fn iter_kind(o: &PyObject) -> (Kind, bool) {
    let t = o.type_ptr();
    for k in [Kind::Keys, Kind::Values, Kind::Items] {
        if t == iter_tp(k, false) {
            return (k, false);
        }
        if t == iter_tp(k, true) {
            return (k, true);
        }
    }
    (Kind::Keys, false)
}

pub fn new_dict_iter(d: &PyObject, k: Kind, rev: bool) -> PyResult {
    unsafe {
        let op = crate::alloc::alloc_object(iter_tp(k, rev), std::mem::size_of::<DictIter>());
        let it = &mut *(op as *mut DictIter);
        it.dict = d.to_ref().into_raw();
        it.used = dict::dict_len(d) as isize;
        it.len = it.used;
        it.pos = if rev { dict::nentries(d) as isize - 1 } else { 0 };
        if rev && it.used == 0 {
            it.pos = -1;
        }
        crate::gc::track(op);
        Ok(PyRef::from_raw(op))
    }
}
unsafe extern "C" fn iter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut DictIter)).dict);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn iter_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let d = (*(o as *mut DictIter)).dict;
        if d.is_null() { 0 } else { visit.unwrap()(d, arg) }
    }
}
unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

fn finish(it: &mut DictIter) {
    let d = std::mem::replace(&mut it.dict, std::ptr::null_mut());
    unsafe { xdecref(d) };
}

fn iter_next_rs(o: &PyObject) -> PyResult<Option<PyRef>> {
    // shared between threads: one thread may drop the exhausted dict while another reads it
    let _g = crate::objlock::lock_held(o);
    let it = unsafe { &mut *(o.as_ptr() as *mut DictIter) };
    if it.dict.is_null() {
        return Ok(None);
    }
    let d = unsafe { &*it.dict };
    if it.used != dict::dict_len(d) as isize {
        it.used = -1;
        raise!(RuntimeError, "dictionary changed size during iteration");
    }
    let (k, rev) = iter_kind(o);
    let found = {
        let _g = dict::ob_lock(d);
        let t = dict::table(d);
        let n = t.entries().len() as isize;
        let mut i = it.pos;
        let mut res = None;
        if rev {
            if i < n {
                while i >= 0 && t.entries()[i as usize].key.is_null() {
                    i -= 1;
                }
                if i >= 0 {
                    let e = t.entries()[i as usize];
                    res = Some(unsafe { (PyRef::from_borrowed(e.key), PyRef::from_borrowed(e.value)) });
                    it.pos = i - 1;
                }
            }
        } else {
            while i < n && t.entries()[i as usize].key.is_null() {
                i += 1;
            }
            if i < n {
                let e = t.entries()[i as usize];
                res = Some(unsafe { (PyRef::from_borrowed(e.key), PyRef::from_borrowed(e.value)) });
                it.pos = i + 1;
            }
        }
        res
    };
    match found {
        None => {
            finish(it);
            Ok(None)
        }
        Some((key, value)) => {
            if it.len == 0 {
                finish(it);
                raise!(RuntimeError, "dictionary keys changed during iteration");
            }
            it.len -= 1;
            Ok(Some(match k {
                Kind::Keys => key,
                Kind::Values => value,
                Kind::Items => tuple::tuple2(key, value),
            }))
        }
    }
}
unsafe extern "C" fn iter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        match iter_next_rs(&*o) {
            Ok(Some(v)) => v.into_raw(),
            _ => std::ptr::null_mut(),
        }
    }
}
fn iter_length_hint(o: &PyObject) -> PyResult {
    let it = unsafe { &*(o.as_ptr() as *mut DictIter) };
    let n = if !it.dict.is_null() && it.used == dict::dict_len(unsafe { &*it.dict }) as isize { it.len } else { 0 };
    Ok(int::new_int(n as i64))
}
fn iter_reduce(o: &PyObject) -> PyResult {
    let iter_fn = crate::builtins::eval_builtin("iter")?;
    let it = unsafe { &*(o.as_ptr() as *mut DictIter) };
    // Drain a copy of the iterator.
    let items = if it.dict.is_null() {
        Vec::new()
    } else {
        let tp = o.type_ptr();
        let cp = unsafe {
            let op = crate::alloc::alloc_object(tp, std::mem::size_of::<DictIter>());
            let c = &mut *(op as *mut DictIter);
            c.dict = (*it.dict).to_ref().into_raw();
            c.used = it.used;
            c.pos = it.pos;
            c.len = it.len;
            crate::gc::track(op);
            PyRef::from_raw(op)
        };
        let mut v = Vec::new();
        while let Some(x) = iter_next_rs(&cp)? {
            v.push(x);
        }
        v
    };
    Ok(tuple::new_tuple(vec![iter_fn, tuple::new_tuple(vec![list::new_list(items)])]))
}

static mut VIEW_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut SET_VIEW_NUM: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    unsafe {
        let s = &mut *(&raw mut VIEW_SEQ);
        s.sq_length = Some(view_len);
        s.sq_contains = Some(view_contains);
        let n = &mut *(&raw mut SET_VIEW_NUM);
        n.nb_subtract = Some(crate::methods::binary_shim(view_sub));
        n.nb_and = Some(crate::methods::binary_shim(view_and));
        n.nb_or = Some(crate::methods::binary_shim(view_or));
        n.nb_xor = Some(crate::methods::binary_shim(view_xor));
        for (k, nm) in [(Kind::Keys, c"dict_keys"), (Kind::Values, c"dict_values"), (Kind::Items, c"dict_items")] {
            let t = &mut *view_tp(k);
            t.tp_name = nm.as_ptr();
            t.tp_basicsize = std::mem::size_of::<DictView>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
            t.tp_dealloc = Some(view_dealloc);
            t.tp_traverse = Some(view_traverse);
            t.tp_iter = Some(view_iter);
            t.tp_repr = Some(view_repr);
            t.tp_as_sequence = s;
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_getset = crate::getset_table![("mapping", mapping; "dictionary that this view refers to")];
            if k == Kind::Values {
                t.tp_methods = crate::mdefs![(
                    "__reversed__",
                    NOARGS,
                    view_reversed,
                    "Return a reverse iterator over the dict values."
                )];
            } else {
                t.tp_richcompare = Some(view_richcompare);
                t.tp_hash = Some(crate::types::slots::hash_not_implemented);
                t.tp_as_number = n;
                t.tp_methods = crate::mdefs![
                    ("isdisjoint", O, view_isdisjoint, "Return True if the view and the given iterable have a null intersection."),
                    ("__reversed__", NOARGS, view_reversed, "Return a reverse iterator over the dict keys."),
                ];
            }
        }
        for k in [Kind::Keys, Kind::Values, Kind::Items] {
            for rev in [false, true] {
                let t = &mut *iter_tp(k, rev);
                t.tp_name = match (k, rev) {
                    (Kind::Keys, false) => c"dict_keyiterator",
                    (Kind::Values, false) => c"dict_valueiterator",
                    (Kind::Items, false) => c"dict_itemiterator",
                    (Kind::Keys, true) => c"dict_reversekeyiterator",
                    (Kind::Values, true) => c"dict_reversevalueiterator",
                    (Kind::Items, true) => c"dict_reverseitemiterator",
                }
                .as_ptr();
                t.tp_basicsize = std::mem::size_of::<DictIter>() as isize;
                t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
                t.tp_dealloc = Some(iter_dealloc);
                t.tp_traverse = Some(iter_traverse);
                t.tp_iter = Some(self_iter);
                t.tp_iternext = Some(iter_next);
                t.tp_getattro = Some(object_generic_getattro);
                t.tp_methods = crate::mdefs![
                    ("__length_hint__", NOARGS, iter_length_hint, "Private method returning an estimate of len(list(it))."),
                    ("__reduce__", NOARGS, iter_reduce, "Return state information for pickling."),
                ];
            }
        }
    }
}

pub fn ready() -> PyResult<()> {
    for k in [Kind::Keys, Kind::Values, Kind::Items] {
        type_ready(view_tp(k))?;
        type_ready(iter_tp(k, false))?;
        type_ready(iter_tp(k, true))?;
    }
    Ok(())
}
