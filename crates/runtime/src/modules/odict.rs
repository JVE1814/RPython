//! collections.OrderedDict as a native dict subtype (grind-lib).
//!
//! The values live in the dict part; the order is a doubly linked list of
//! nodes, one per key.  To find the node of a key without a second table, a
//! side array maps the dict's entry index of the key to its node.  Entry
//! indices stay put until the dict's storage is rebuilt (resize, clear), which
//! is detected by comparing the dict's entry array pointer with the one the
//! side array was built for; the array is then rebuilt from the list using
//! the hashes remembered in the nodes.
//!
//! Every structural change of the list bumps `state`, which iterators,
//! `__eq__` and `copy()` use to detect changes made behind their back.
//! Operations through `dict` methods (dict.__setitem__(od, ...)) bypass the
//! list; the code copes with keys that have no node and nodes whose key is no
//! longer in the dict (KeyError where a value is needed).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, targs};
use crate::object::*;
use crate::types::dict;
use crate::types::dictview;
use crate::types::typeobject::{is_subtype, type_ref};
use crate::types::{bool_, int, list, none, str as pystr, tuple};
use crate::{mdefs, raise};
use std::ffi::{c_int, c_void};

#[unsafe(no_mangle)]
pub static mut PyODict_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyODictKeys_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyODictValues_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyODictItems_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyODictIter_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn odict_type() -> *mut PyTypeObject {
    &raw mut PyODict_Type
}
fn iter_type() -> *mut PyTypeObject {
    &raw mut PyODictIter_Type
}

/// Instance layout: the dict followed by the fields of the C implementation's
/// odict object (so that the object size matches).
#[repr(C)]
pub struct ODict {
    dict: PyDictObject,
    first: *mut Node,
    last: *mut Node,
    /// entry index -> node (a leaked Box<Vec>)
    fast_nodes: *mut Vec<*mut Node>,
    fast_nodes_size: isize,
    /// the dict entry array the side array was built for
    resize_sentinel: *mut c_void,
    state: usize,
    inst_dict: *mut PyObject,
    weakreflist: *mut PyObject,
}

struct Node {
    key: *mut PyObject, // owned
    hash: isize,
    next: *mut Node,
    prev: *mut Node,
    /// entry index this node is filed under in the side array
    ix: usize,
}

/// The reported per-node size (key, hash, next, prev).
const NODE_SIZE: usize = 4 * 8;

fn od(o: &PyObject) -> &mut ODict {
    unsafe { &mut *(o.as_ptr() as *mut ODict) }
}

pub fn is_odict(o: &PyObject) -> bool {
    o.is_type(odict_type()) || is_subtype(o.type_ptr(), odict_type())
}

fn mutated() -> PyErr {
    err!(RuntimeError, "OrderedDict mutated during iteration")
}

fn key_error(key: &PyObject) -> PyErr {
    set_err_obj(crate::types::exceptions::KeyError(), key.to_ref())
}

// ------------------------------------------------------------------ list + index

fn entries_ptr(o: &PyObject) -> *mut c_void {
    dict::table(o).entries_p as *mut c_void
}

fn side(o: &PyObject) -> &mut Vec<*mut Node> {
    let d = od(o);
    if d.fast_nodes.is_null() {
        d.fast_nodes = Box::into_raw(Box::new(Vec::new()));
    }
    unsafe { &mut *d.fast_nodes }
}

/// Rebuild the entry-index -> node array from the list.
fn rebuild_index(o: &PyObject) -> PyResult<()> {
    let state0 = od(o).state;
    let mut v: Vec<*mut Node> = Vec::new();
    let mut n = od(o).first;
    while !n.is_null() {
        let node = unsafe { &mut *n };
        let key = unsafe { (*node.key).to_ref() };
        if let Some(ix) = dict::lookup_index(o, &key, node.hash)? {
            if od(o).state != state0 {
                // a key's __eq__ changed the list: start over
                return rebuild_index(o);
            }
            if v.len() <= ix {
                v.resize(ix + 1, std::ptr::null_mut());
            }
            v[ix] = n;
            node.ix = ix;
        } else {
            node.ix = usize::MAX;
        }
        n = node.next;
    }
    *side(o) = v;
    od(o).resize_sentinel = entries_ptr(o);
    od(o).fast_nodes_size = dict::index_slots(o) as isize;
    Ok(())
}

fn ensure_index(o: &PyObject) -> PyResult<()> {
    if od(o).fast_nodes.is_null() || od(o).resize_sentinel != entries_ptr(o) {
        rebuild_index(o)?;
    }
    Ok(())
}

/// The node of `key` (hash `h`), or null.
fn find_node(o: &PyObject, key: &PyObject, h: isize) -> PyResult<*mut Node> {
    for _ in 0..3 {
        ensure_index(o)?;
        let ix = match dict::lookup_index(o, key, h)? {
            Some(ix) => ix,
            None => return Ok(std::ptr::null_mut()),
        };
        if od(o).resize_sentinel != entries_ptr(o) {
            continue; // the lookup ran code that rebuilt the dict
        }
        let n = side(o).get(ix).copied().unwrap_or(std::ptr::null_mut());
        if n.is_null() {
            return Ok(n);
        }
        let entry_key = dict::table(o).entries().get(ix).map(|e| e.key).unwrap_or(std::ptr::null_mut());
        if unsafe { (*n).key } == entry_key {
            return Ok(n);
        }
        // stale (the dict was emptied in place): rebuild and retry
        rebuild_index(o)?;
    }
    Ok(std::ptr::null_mut())
}

fn link_tail(o: &PyObject, n: *mut Node) {
    let d = od(o);
    unsafe {
        (*n).next = std::ptr::null_mut();
        (*n).prev = d.last;
        if d.last.is_null() {
            d.first = n;
        } else {
            (*d.last).next = n;
        }
        d.last = n;
    }
    d.state = d.state.wrapping_add(1);
}

fn link_head(o: &PyObject, n: *mut Node) {
    let d = od(o);
    unsafe {
        (*n).prev = std::ptr::null_mut();
        (*n).next = d.first;
        if d.first.is_null() {
            d.last = n;
        } else {
            (*d.first).prev = n;
        }
        d.first = n;
    }
    d.state = d.state.wrapping_add(1);
}

fn unlink(o: &PyObject, n: *mut Node) {
    let d = od(o);
    unsafe {
        let (p, nx) = ((*n).prev, (*n).next);
        if p.is_null() {
            d.first = nx;
        } else {
            (*p).next = nx;
        }
        if nx.is_null() {
            d.last = p;
        } else {
            (*nx).prev = p;
        }
        (*n).prev = std::ptr::null_mut();
        (*n).next = std::ptr::null_mut();
    }
    d.state = d.state.wrapping_add(1);
}

/// Unlink and free a node (dropping its key reference).
fn remove_node(o: &PyObject, n: *mut Node) {
    unlink(o, n);
    unsafe {
        let ix = (*n).ix;
        if !od(o).fast_nodes.is_null() {
            if let Some(slot) = side(o).get_mut(ix) {
                if *slot == n {
                    *slot = std::ptr::null_mut();
                }
            }
        }
        let b = Box::from_raw(n);
        xdecref(b.key);
    }
}

/// Make sure `key` (already in the dict) has a node; append one if not.
fn add_node(o: &PyObject, key: &PyObject, h: isize) -> PyResult<()> {
    if !find_node(o, key, h)?.is_null() {
        return Ok(());
    }
    let ix = match dict::lookup_index(o, key, h)? {
        Some(ix) => ix,
        None => return Ok(()), // removed again by a key's __eq__
    };
    let n = Box::into_raw(Box::new(Node { key: key.to_ref().into_raw(), hash: h, next: std::ptr::null_mut(), prev: std::ptr::null_mut(), ix }));
    link_tail(o, n);
    if od(o).resize_sentinel == entries_ptr(o) {
        let v = side(o);
        if v.len() <= ix {
            v.resize(ix + 1, std::ptr::null_mut());
        }
        v[ix] = n;
    }
    Ok(())
}

/// Free every node.
fn clear_nodes(o: &PyObject) {
    let d = od(o);
    let mut n = d.first;
    d.first = std::ptr::null_mut();
    d.last = std::ptr::null_mut();
    while !n.is_null() {
        let b = unsafe { Box::from_raw(n) };
        n = b.next;
        unsafe { xdecref(b.key) };
    }
    if !d.fast_nodes.is_null() {
        unsafe { drop(Box::from_raw(d.fast_nodes)) };
        d.fast_nodes = std::ptr::null_mut();
    }
    d.resize_sentinel = std::ptr::null_mut();
    d.fast_nodes_size = 0;
    d.state = d.state.wrapping_add(1);
}

fn node_key(n: *mut Node) -> PyRef {
    unsafe { (*n.cast::<Node>()).key.as_ref().unwrap().to_ref() }
}

/// Keys in list order (a snapshot).
fn keys_snapshot(o: &PyObject) -> Vec<PyRef> {
    let mut v = Vec::new();
    let mut n = od(o).first;
    while !n.is_null() {
        v.push(node_key(n));
        n = unsafe { (*n).next };
    }
    v
}

/// The value of `key` straight from the dict (fresh hash); KeyError if absent.
fn dict_value(o: &PyObject, key: &PyObject) -> PyResult<PyRef> {
    match dict::get_item(o, key)? {
        Some(v) => Ok(v),
        None => Err(key_error(key)),
    }
}

// ------------------------------------------------------------------ core operations

/// od[key] = value
pub fn setitem(o: &PyObject, key: &PyObject, value: &PyObject) -> PyResult<()> {
    let h = dict::hash_key(key, "dict key")?;
    dict::set_item_hash(o, key, h, value)?;
    if let Err(e) = add_node(o, key, h) {
        let saved = take_exc();
        let _ = dict::del_item_hash(o, key, h);
        clear_exc();
        if let Some(s) = saved {
            set_exc(s);
        }
        return Err(e);
    }
    Ok(())
}

/// del od[key]
pub fn delitem(o: &PyObject, key: &PyObject) -> PyResult<()> {
    let h = dict::hash_key(key, "dict key")?;
    let n = find_node(o, key, h)?;
    if !n.is_null() {
        remove_node(o, n);
    }
    if !dict::del_item_hash(o, key, h)? {
        return Err(key_error(key));
    }
    Ok(())
}

/// Remove `key` and return its value (None if absent), by the given hash.
fn pop_hash(o: &PyObject, key: &PyObject, h: isize) -> PyResult<Option<PyRef>> {
    let n = find_node(o, key, h)?;
    if n.is_null() {
        // the key may still be in the dict (added through dict methods)
        return dict::pop_item_hash(o, key, h);
    }
    remove_node(o, n);
    dict::pop_item_hash(o, key, h)
}

// ------------------------------------------------------------------ slots

unsafe extern "C" fn od_ass_sub(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let o = &*o;
        // the node list and side array are not safe to change from two threads
        let _lk = crate::objlock::lock_held(o);
        let r = if v.is_null() { delitem(o, &*k) } else { setitem(o, &*k, &*v) };
        int_into_c(r)
    }
}

/// Update from a mapping or an iterable of pairs, then from keywords, using
/// the object's own __setitem__ (MutableMapping.update semantics).
fn mapping_update(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>, fname: &str) -> PyResult<()> {
    if args.len() > 1 {
        raise!(TypeError, "{} expected at most 1 argument, got {}", fname, args.len());
    }
    let set = |k: &PyObject, v: &PyObject| -> PyResult<()> {
        if o.is_type(odict_type()) { setitem(o, k, v) } else { ab::setitem(o, k, Some(v)) }
    };
    if let Some(&other) = args.first() {
        if dict::is_dict_exact(other) {
            for (k, v) in dict::items_vec(other) {
                set(&k, &v)?;
            }
        } else if let Some(keys) = ab::getattr_opt(other, crate::name!("keys"))? {
            let ks = ab::call(&keys, &[])?;
            let it = ab::get_iter(&ks)?;
            while let Some(k) = ab::iter_next(&it)? {
                let v = ab::getitem(other, &k)?;
                set(&k, &v)?;
            }
        } else if let Some(items) = ab::getattr_opt(other, &pystr::intern_str("items"))? {
            let its = ab::call(&items, &[])?;
            let it = ab::get_iter(&its)?;
            while let Some(pair) = ab::iter_next(&it)? {
                let kv = ab::collect_iterable(&pair)?;
                if kv.len() != 2 {
                    raise!(ValueError, "need 2 values to unpack, got {}", kv.len());
                }
                set(&kv[0], &kv[1])?;
            }
        } else {
            let it = ab::get_iter(other)?;
            while let Some(pair) = ab::iter_next(&it)? {
                let kv = match ab::collect_iterable(&pair) {
                    Ok(v) => v,
                    Err(e) => {
                        if exc_matches(crate::types::exceptions::TypeError()) {
                            clear_exc();
                            raise!(TypeError, "cannot convert dictionary update sequence element to a sequence");
                        }
                        return Err(e);
                    }
                };
                if kv.len() != 2 {
                    raise!(ValueError, "dictionary update sequence element has length {}; 2 is required", kv.len());
                }
                set(&kv[0], &kv[1])?;
            }
        }
    }
    if let Some(kw) = kw {
        for (k, v) in dict::items_vec(kw) {
            set(&k, &v)?;
        }
    }
    Ok(())
}

unsafe extern "C" fn od_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let kw = fc::opt(kw).filter(|k| dict::dict_len(k) > 0);
        int_into_c(mapping_update(&*o, targs(args), kw, "OrderedDict"))
    }
}

unsafe extern "C" fn od_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let _lk = crate::objlock::lock_held(&*o);
        into_c(new_iter(&*o, KIND_KEYS, false))
    }
}

unsafe extern "C" fn od_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        into_c((|| -> PyResult {
            let name = fc::repr_type_name(o);
            if dict::dict_len(o) == 0 {
                return Ok(pystr::new_str(&format!("{}()", name)));
            }
            let g = match fc::ReprGuard::enter(o) {
                Some(g) => g,
                None => return Ok(pystr::new_str("...")),
            };
            let pairs: Vec<(PyRef, PyRef)> = if o.is_type(odict_type()) {
                let mut v = Vec::new();
                for k in keys_snapshot(o) {
                    let val = dict_value(o, &k)?;
                    v.push((k, val));
                }
                v
            } else {
                let items = ab::call_method_str(o, "items", &[])?;
                let mut v = Vec::new();
                let it = ab::get_iter(&items)?;
                while let Some(p) = ab::iter_next(&it)? {
                    let kv = ab::collect_iterable(&p)?;
                    if kv.len() == 2 {
                        v.push((kv[0].clone(), kv[1].clone()));
                    }
                }
                v
            };
            if pairs.is_empty() {
                drop(g);
                return Ok(pystr::new_str(&format!("{}()", name)));
            }
            let mut s = String::new();
            s.push_str(&name);
            s.push_str("({");
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&ab::repr_string(k)?);
                s.push_str(": ");
                s.push_str(&ab::repr_string(v)?);
            }
            s.push_str("})");
            drop(g);
            Ok(pystr::new_str(&s))
        })())
    }
}

/// Compare the key orders of two OrderedDicts (after the dicts compared equal).
fn keys_equal(a: &PyObject, b: &PyObject) -> PyResult<bool> {
    let (mut na, mut nb) = (od(a).first, od(b).first);
    loop {
        if na.is_null() {
            return Ok(nb.is_null());
        }
        if nb.is_null() {
            return Ok(false);
        }
        let (sa, sb) = (od(a).state, od(b).state);
        let (ka, kb) = (node_key(na), node_key(nb));
        let eq = ab::rich_compare_bool(&ka, &kb, Py_EQ)?;
        if od(a).state != sa || od(b).state != sb {
            return Err(mutated());
        }
        if !eq {
            return Ok(false);
        }
        unsafe {
            na = (*na).next;
            nb = (*nb).next;
        }
    }
}

unsafe extern "C" fn od_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let base = (*dict::dict_type()).tp_richcompare.unwrap();
        let (ra, rb) = (&*a, &*b);
        if (op != Py_EQ && op != Py_NE) || !is_odict(ra) || !is_odict(rb) {
            return base(a, b, op);
        }
        into_c((|| -> PyResult {
            let r = from_c(base(a, b, Py_EQ))?;
            if r.is(none::not_implemented()) {
                return Ok(r);
            }
            let mut eq = ab::is_true(&r)?;
            if eq {
                eq = keys_equal(ra, rb)?;
            }
            Ok(bool_::new_bool(if op == Py_EQ { eq } else { !eq }))
        })())
    }
}

unsafe extern "C" fn od_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let o = &*op;
        if !od(o).weakreflist.is_null() {
            crate::types::weakref::clear_weakrefs_at(op, &raw mut od(o).weakreflist);
        }
        clear_nodes(o);
        fc::clear_field(&raw mut od(o).inst_dict);
        (*dict::dict_type()).tp_dealloc.unwrap()(op);
    }
}

unsafe extern "C" fn od_traverse(op: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let o = &*op;
        let r = fc::visit(od(o).inst_dict, v, arg);
        if r != 0 {
            return r;
        }
        let mut n = od(o).first;
        while !n.is_null() {
            let r = fc::visit((*n).key, v, arg);
            if r != 0 {
                return r;
            }
            n = (*n).next;
        }
        match (*dict::dict_type()).tp_traverse {
            Some(t) => t(op, v, arg),
            None => 0,
        }
    }
}

unsafe extern "C" fn od_clear_slot(op: *mut PyObject) -> c_int {
    unsafe {
        let o = &*op;
        fc::clear_field(&raw mut od(o).inst_dict);
        clear_nodes(o);
        match (*dict::dict_type()).tp_clear {
            Some(c) => c(op),
            None => 0,
        }
    }
}

/// The type used for `a | b` results and copies.
fn copy_of(o: &PyObject, src: &PyObject) -> PyResult {
    ab::call(&type_ref(o.type_ptr()), &[src])
}

unsafe extern "C" fn od_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        into_c((|| -> PyResult {
            if !dict::is_anydict(a) || !dict::is_anydict(b) {
                return Ok(none::not_implemented().to_ref());
            }
            let (new, other) = if is_odict(a) { (copy_of(a, a)?, b) } else { (copy_of(b, a)?, b) };
            mapping_update(&new, &[other], None, "update")?;
            Ok(new)
        })())
    }
}

unsafe extern "C" fn od_ior(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        into_c(mapping_update(a, &[b], None, "update").map(|_| a.to_ref()))
    }
}

// ------------------------------------------------------------------ methods

fn m_setdefault(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [key, default] = crate::modules::posix::kwparse::<2>("setdefault", &["key", "default"], 0, 1, 0, args, kw)?;
    let key = key.unwrap();
    let default = default.map(|d| d.to_ref()).unwrap_or_else(none::none_ref);
    if o.is_type(odict_type()) {
        if let Some(v) = dict::get_item(o, key)? {
            return Ok(v);
        }
        setitem(o, key, &default)?;
        return Ok(default);
    }
    if ab::contains(o, key)? {
        return ab::getitem(o, key);
    }
    ab::setitem(o, key, Some(&default))?;
    Ok(default)
}

fn m_pop(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [key, default] = crate::modules::posix::kwparse::<2>("pop", &["key", "default"], 0, 1, 0, args, kw)?;
    let key = key.unwrap();
    let h = dict::hash_key(key, "dict key")?;
    match pop_hash(o, key, h)? {
        Some(v) => Ok(v),
        None => match default {
            Some(d) => Ok(d.to_ref()),
            None => Err(key_error(key)),
        },
    }
}

fn m_popitem(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [last] = crate::modules::posix::kwparse::<1>("popitem", &["last"], 0, 0, 0, args, kw)?;
    let last = match last {
        Some(l) => ab::is_true(l)?,
        None => true,
    };
    let n = if last { od(o).last } else { od(o).first };
    if n.is_null() {
        raise!(KeyError, "dictionary is empty");
    }
    let key = node_key(n);
    let h = unsafe { (*n).hash };
    remove_node(o, n);
    match dict::pop_item_hash(o, &key, h)? {
        Some(v) => Ok(tuple::tuple2(key, v)),
        None => Err(key_error(&key)),
    }
}

fn m_move_to_end(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [key, last] = crate::modules::posix::kwparse::<2>("move_to_end", &["key", "last"], 0, 1, 0, args, kw)?;
    let key = key.unwrap();
    let last = match last {
        Some(l) => ab::is_true(l)?,
        None => true,
    };
    if od(o).first.is_null() {
        return Err(key_error(key));
    }
    let h = dict::hash_key(key, "dict key")?;
    let n = find_node(o, key, h)?;
    if n.is_null() {
        return Err(key_error(key));
    }
    if last {
        if n != od(o).last {
            unlink(o, n);
            link_tail(o, n);
        }
    } else if n != od(o).first {
        unlink(o, n);
        link_head(o, n);
    }
    Ok(none::none_ref())
}

fn m_clear(o: &PyObject) -> PyResult {
    dict::clear(o);
    clear_nodes(o);
    Ok(none::none_ref())
}

fn m_copy(o: &PyObject) -> PyResult {
    let new = ab::call(&type_ref(o.type_ptr()), &[])?;
    let exact = o.is_type(odict_type());
    let state0 = od(o).state;
    let mut n = od(o).first;
    while !n.is_null() {
        let key = node_key(n);
        let value = if exact { dict_value(o, &key)? } else { ab::getitem(o, &key)? };
        if od(o).state != state0 {
            return Err(mutated());
        }
        if is_odict(&new) && new.is_type(odict_type()) {
            setitem(&new, &key, &value)?;
        } else {
            ab::setitem(&new, &key, Some(&value))?;
        }
        if od(o).state != state0 {
            return Err(mutated());
        }
        n = unsafe { (*n).next };
    }
    Ok(new)
}

fn m_update(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = crate::args::split(args, kw);
    let kwd = if kws.is_empty() {
        None
    } else {
        let d = dict::new_dict();
        for (k, v) in kws {
            dict::set_item(&d, k, v)?;
        }
        Some(d)
    };
    mapping_update(o, pos, kwd.as_deref(), "update")?;
    Ok(none::none_ref())
}

fn m_fromkeys(cls: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [iterable, value] = crate::modules::posix::kwparse::<2>("fromkeys", &["iterable", "value"], 0, 1, 0, args, kw)?;
    let value = value.map(|v| v.to_ref()).unwrap_or_else(none::none_ref);
    let new = ab::call(cls, &[])?;
    let it = ab::get_iter(iterable.unwrap())?;
    while let Some(k) = ab::iter_next(&it)? {
        ab::setitem(&new, &k, Some(&value))?;
    }
    Ok(new)
}

fn m_keys(o: &PyObject) -> PyResult {
    dictview::new_view_of_type(o, &raw mut PyODictKeys_Type)
}
fn m_values(o: &PyObject) -> PyResult {
    dictview::new_view_of_type(o, &raw mut PyODictValues_Type)
}
fn m_items(o: &PyObject) -> PyResult {
    dictview::new_view_of_type(o, &raw mut PyODictItems_Type)
}
fn m_reversed(o: &PyObject) -> PyResult {
    new_iter(o, KIND_KEYS, true)
}

fn m_sizeof(o: &PyObject) -> PyResult {
    // accounted like the C implementation: dict keys object (header,
    // index bytes, usable entries), the node index and the nodes
    // (the table size is the one the C dict would have for these entries)
    let mut n = o.ty().tp_basicsize as usize;
    let used = dict::nentries(o).max(dict::dict_len(o));
    let mut slots = 8usize;
    while slots * 2 / 3 < used {
        slots <<= 1;
    }
    if used > 0 || !od(o).first.is_null() {
        let ix_bytes = if slots <= 0xff { 1 } else if slots <= 0xffff { 2 } else if slots <= 0xffff_ffff { 4 } else { 8 };
        n += 32 + slots * ix_bytes + (slots * 2 / 3) * 24;
    }
    if !od(o).fast_nodes.is_null() || !od(o).first.is_null() {
        n += slots * 8;
    }
    let mut c = 0;
    let mut p = od(o).first;
    while !p.is_null() {
        c += 1;
        p = unsafe { (*p).next };
    }
    n += c * NODE_SIZE;
    Ok(int::new_int(n as i64))
}

fn m_reduce(o: &PyObject) -> PyResult {
    let tp = type_ref(o.type_ptr());
    let mut state = ab::call_method_str(o, "__getstate__", &[])?;
    if dict::is_dict(&state) && dict::dict_len(&state) == 0 {
        state = none::none_ref();
    }
    let items = m_items(o)?;
    let it = ab::get_iter(&items)?;
    Ok(tuple::new_tuple(vec![tp, tuple::empty_tuple(), state, none::none_ref(), it]))
}

// ------------------------------------------------------------------ views

unsafe extern "C" fn keys_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(new_iter(dictview::view_dict(&*o), KIND_KEYS, false)) }
}
unsafe extern "C" fn values_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(new_iter(dictview::view_dict(&*o), KIND_VALUES, false)) }
}
unsafe extern "C" fn items_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(new_iter(dictview::view_dict(&*o), KIND_ITEMS, false)) }
}
fn keys_reversed(o: &PyObject) -> PyResult {
    new_iter(dictview::view_dict(o), KIND_KEYS, true)
}
fn values_reversed(o: &PyObject) -> PyResult {
    new_iter(dictview::view_dict(o), KIND_VALUES, true)
}
fn items_reversed(o: &PyObject) -> PyResult {
    new_iter(dictview::view_dict(o), KIND_ITEMS, true)
}

// ------------------------------------------------------------------ iterator

const KIND_KEYS: c_int = 1;
const KIND_VALUES: c_int = 2;
const KIND_ITEMS: c_int = 3;
const KIND_REVERSED: c_int = 4;

#[repr(C)]
struct ODIter {
    ob_base: PyObject,
    kind: c_int,
    odict: *mut PyObject,
    size: isize,
    state: usize,
    current: *mut PyObject,
    result: *mut PyObject,
}

fn it(o: &PyObject) -> &mut ODIter {
    unsafe { &mut *(o.as_ptr() as *mut ODIter) }
}

fn new_iter(o: &PyObject, kind: c_int, reversed: bool) -> PyResult {
    unsafe {
        let op = fc::alloc(iter_type())?;
        let i = &mut *(op as *mut ODIter);
        i.kind = kind | if reversed { KIND_REVERSED } else { 0 };
        i.odict = o.to_ref().into_raw();
        i.size = dict::dict_len(o) as isize;
        i.state = od(o).state;
        let start = if reversed { od(o).last } else { od(o).first };
        i.current = if start.is_null() { std::ptr::null_mut() } else { node_key(start).into_raw() };
        i.result = std::ptr::null_mut();
        crate::gc::track(op);
        Ok(PyRef::from_raw(op))
    }
}

/// Advance; returns the key just passed (None at the end).
fn iter_next_key(i: &mut ODIter) -> PyResult<Option<PyRef>> {
    if i.current.is_null() || i.odict.is_null() {
        return Ok(None);
    }
    let o = unsafe { (*i.odict).to_ref() };
    if od(&o).state != i.state {
        unsafe { fc::clear_field(&raw mut i.current) };
        return Err(mutated());
    }
    if dict::dict_len(&o) as isize != i.size {
        unsafe { fc::clear_field(&raw mut i.current) };
        raise!(RuntimeError, "OrderedDict changed size during iteration");
    }
    let key = unsafe { (*i.current).to_ref() };
    let h = dict::hash_key(&key, "dict key")?;
    let n = find_node(&o, &key, h)?;
    if n.is_null() {
        unsafe { fc::clear_field(&raw mut i.current) };
        return Err(key_error(&key));
    }
    let next = unsafe { if i.kind & KIND_REVERSED != 0 { (*n).prev } else { (*n).next } };
    let nk = if next.is_null() { None } else { Some(node_key(next)) };
    unsafe { fc::set_field(&raw mut i.current, nk) };
    Ok(Some(key))
}

unsafe extern "C" fn iter_next(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        // iterator, then the OrderedDict (whose node list it walks)
        let _li = crate::objlock::lock_held(&*op);
        let i = it(&*op);
        // (no extra reference: when the dict dies is observable through __del__; the
        // lock is keyed by address and stays valid even if the dict goes meanwhile)
        let _lo = if i.odict.is_null() { None } else { Some(crate::objlock::lock_held(&*i.odict)) };
        let r = (|| -> PyResult<Option<PyRef>> {
            let key = match iter_next_key(i)? {
                Some(k) => k,
                None => return Ok(None),
            };
            let kind = i.kind & !KIND_REVERSED;
            if kind == KIND_KEYS {
                return Ok(Some(key));
            }
            let o = (*i.odict).to_ref();
            let value = dict_value(&o, &key)?;
            if kind == KIND_VALUES {
                return Ok(Some(value));
            }
            Ok(Some(tuple::tuple2(key, value)))
        })();
        match r {
            Ok(Some(v)) => v.into_raw(),
            Ok(None) => std::ptr::null_mut(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}

fn iter_reduce(o: &PyObject) -> PyResult {
    // the remaining elements, without advancing this iterator
    let i = it(o);
    let mut rest = Vec::new();
    if !i.current.is_null() && !i.odict.is_null() {
        let d = unsafe { (*i.odict).to_ref() };
        let key = unsafe { (*i.current).to_ref() };
        let h = dict::hash_key(&key, "dict key")?;
        let mut n = find_node(&d, &key, h)?;
        let kind = i.kind & !KIND_REVERSED;
        while !n.is_null() {
            let k = node_key(n);
            rest.push(match kind {
                KIND_KEYS => k,
                KIND_VALUES => dict_value(&d, &k)?,
                _ => {
                    let v = dict_value(&d, &k)?;
                    tuple::tuple2(k, v)
                }
            });
            n = unsafe { if i.kind & KIND_REVERSED != 0 { (*n).prev } else { (*n).next } };
        }
    }
    let iter = crate::types::module::module_dict(&crate::builtins::builtins_module());
    let iter = dict::get_item_str(&iter, crate::name!("iter")).unwrap();
    Ok(tuple::new_tuple(vec![iter, tuple::new_tuple(vec![list::new_list(rest)])]))
}

fn iter_length_hint(o: &PyObject) -> PyResult {
    let i = it(o);
    if i.odict.is_null() || i.current.is_null() {
        return Ok(int::new_int(0));
    }
    Ok(int::new_int(dict::dict_len(unsafe { &*i.odict }) as i64))
}

unsafe extern "C" fn iter_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let i = it(&*op);
        fc::clear_field(&raw mut i.odict);
        fc::clear_field(&raw mut i.current);
        fc::clear_field(&raw mut i.result);
        fc::free(op);
    }
}
unsafe extern "C" fn iter_traverse(op: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let i = it(&*op);
        for p in [i.odict, i.current, i.result] {
            let r = fc::visit(p, v, arg);
            if r != 0 {
                return r;
            }
        }
        0
    }
}
unsafe extern "C" fn iter_self(op: *mut PyObject) -> *mut PyObject {
    unsafe { (*op).to_ref().into_raw() }
}

// ------------------------------------------------------------------ type setup

fn leak<T>(v: T) -> *mut T {
    Box::into_raw(Box::new(v))
}

pub fn init_types() -> PyResult<()> {
    unsafe {
        if PyODict_Type.tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        let t = odict_type();
        fc::init_type(
            t,
            c"collections.OrderedDict",
            std::mem::size_of::<ODict>(),
            Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_DICT_SUBCLASS | Py_TPFLAGS_MAPPING,
        );
        let dt = dict::dict_type();
        (*t).tp_base = dt;
        (*t).tp_alloc = None;
        (*t).tp_free = None;
        (*t).tp_dealloc = Some(od_dealloc);
        (*t).tp_traverse = Some(od_traverse);
        (*t).tp_clear = Some(od_clear_slot);
        (*t).tp_init = Some(od_init);
        (*t).tp_repr = Some(od_repr);
        (*t).tp_iter = Some(od_iter);
        (*t).tp_richcompare = Some(od_richcompare);
        (*t).tp_hash = (*dt).tp_hash;
        (*t).tp_dictoffset = std::mem::offset_of!(ODict, inst_dict) as isize;
        (*t).tp_weaklistoffset = std::mem::offset_of!(ODict, weakreflist) as isize;
        let mut mp: PyMappingMethods = std::mem::zeroed();
        if !(*dt).tp_as_mapping.is_null() {
            mp = std::ptr::read((*dt).tp_as_mapping);
        }
        mp.mp_ass_subscript = Some(od_ass_sub);
        (*t).tp_as_mapping = leak(mp);
        let mut nb: PyNumberMethods = std::mem::zeroed();
        nb.nb_or = Some(od_or);
        nb.nb_inplace_or = Some(od_ior);
        (*t).tp_as_number = leak(nb);
        (*t).tp_doc = c"Dictionary that remembers insertion order".as_ptr();
        (*t).tp_methods = mdefs![
            ("__reversed__", NOARGS, m_reversed, "od.__reversed__() <==> reversed(od)"),
            ("__sizeof__", NOARGS, m_sizeof, "D.__sizeof__() -> size of D in memory, in bytes"),
            ("__reduce__", NOARGS, m_reduce, "Return state information for pickling"),
            ("setdefault", FASTKW, m_setdefault, "setdefault($self, /, key, default=None)\n--\n\nInsert key with a value of default if key is not in the dictionary.\n\nReturn the value for key if key is in the dictionary, else default."),
            ("pop", FASTKW, m_pop, "pop($self, /, key, default=<unrepresentable>)\n--\n\nod.pop(key[,default]) -> v, remove specified key and return the corresponding value.\n\nIf the key is not found, return the default if given; otherwise,\nraise a KeyError."),
            ("popitem", FASTKW, m_popitem, "popitem($self, /, last=True)\n--\n\nRemove and return a (key, value) pair from the dictionary.\n\nPairs are returned in LIFO order if last is true or FIFO order if false."),
            ("keys", NOARGS, m_keys, "D.keys() -> a set-like object providing a view on D's keys"),
            ("values", NOARGS, m_values, "D.values() -> an object providing a view on D's values"),
            ("items", NOARGS, m_items, "D.items() -> a set-like object providing a view on D's items"),
            ("update", FASTKW, m_update, "D.update([E, ]**F) -> None.  Update D from mapping/iterable E and F."),
            ("clear", NOARGS, m_clear, "od.clear() -> None.  Remove all items from od."),
            ("copy", NOARGS, m_copy, "od.copy() -> a shallow copy of od"),
            ("move_to_end", FASTKW, m_move_to_end, "move_to_end($self, /, key, last=True)\n--\n\nMove an existing element to the end (or beginning if last is false).\n\nRaise KeyError if the element does not exist."),
            ("fromkeys", FASTKW, m_fromkeys, "fromkeys($type, /, iterable, value=None)\n--\n\nCreate a new ordered dictionary with keys from iterable and values set to value."; METH_CLASS),
        ];
        (*t).tp_getset = crate::getset_table![("__dict__", fc::get_dict_attr, fc::set_dict_attr)];

        // views: native subtypes of the dict views, iterating in list order
        for (vt, base, name, iterf, revf) in [
            (&raw mut PyODictKeys_Type, &raw mut dictview::PyDictKeys_Type, c"odict_keys", keys_iter as unsafe extern "C" fn(*mut PyObject) -> *mut PyObject, keys_reversed as fn(&PyObject) -> PyResult),
            (&raw mut PyODictValues_Type, &raw mut dictview::PyDictValues_Type, c"odict_values", values_iter, values_reversed),
            (&raw mut PyODictItems_Type, &raw mut dictview::PyDictItems_Type, c"odict_items", items_iter, items_reversed),
        ] {
            (*vt).tp_name = name.as_ptr();
            (*vt).tp_basicsize = (*base).tp_basicsize;
            (*vt).tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_IMMUTABLETYPE;
            (*vt).tp_base = base;
            (*vt).tp_iter = Some(iterf);
            let m: *mut PyMethodDef = if vt == &raw mut PyODictKeys_Type {
                mdefs![("__reversed__", NOARGS, keys_reversed)]
            } else if vt == &raw mut PyODictValues_Type {
                mdefs![("__reversed__", NOARGS, values_reversed)]
            } else {
                mdefs![("__reversed__", NOARGS, items_reversed)]
            };
            let _ = revf;
            (*vt).tp_methods = m;
            crate::types::typeobject::type_ready(vt)?;
        }

        let t = iter_type();
        fc::init_type(t, c"odict_iterator", std::mem::size_of::<ODIter>(), Py_TPFLAGS_HAVE_GC);
        (*t).tp_dealloc = Some(iter_dealloc);
        (*t).tp_traverse = Some(iter_traverse);
        (*t).tp_iter = Some(iter_self);
        (*t).tp_iternext = Some(iter_next);
        (*t).tp_methods = mdefs![
            ("__reduce__", NOARGS, iter_reduce, "Return state information for pickling."),
            ("__length_hint__", NOARGS, iter_length_hint, "Private method returning an estimate of len(list(it))."),
        ];
        crate::types::typeobject::type_ready(t)?;
        crate::methods::lock_self_methods_held(odict_type());
        crate::types::typeobject::type_ready(odict_type())?;
    }
    Ok(())
}
