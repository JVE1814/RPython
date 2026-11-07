//! `_elementtree`: the accelerator for xml.etree.ElementTree.
//!
//! Provides Element (children kept in a Rust vector, attributes in a lazily
//! created dict), the tree iterators, TreeBuilder, and an XMLParser that runs
//! the XML engine (pyexpat/engine.rs) directly and feeds a target -- with a
//! direct Rust path when the target is our own TreeBuilder.  Path queries
//! that are not a plain child tag are delegated to xml.etree.ElementPath.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::compress_common::ExcCell;
use crate::modules::fncommon::{self as fc, Trav, VcArgs};
use crate::modules::pyexpat::engine::{Engine, Ev, Event};
use crate::modules::pyexpat::xmlparser as px;
use crate::object::*;
use crate::types::typeobject::{type_obj, type_ref};
use crate::types::{bool_, dict, int, list, none, str as pystr, tuple};
use crate::mdefs;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// FNV-1a: names are short and come from one document, so a simple hash
/// beats the default DoS-resistant one.
#[derive(Default)]
struct Fnv(u64);
impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut h = if self.0 == 0 { 0xcbf2_9ce4_8422_2325 } else { self.0 };
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        self.0 = h;
    }
}
type NameHash = BuildHasherDefault<Fnv>;
use std::ffi::c_int;
use std::sync::Mutex;

type VisitFn<'a> = &'a mut dyn FnMut(*mut PyObject) -> c_int;

static mut ELEMENT_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut ITER_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut CHILD_ITER_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut BUILDER_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut PARSER_TYPE: PyTypeObject = PyTypeObject::ZERO;
static PARSE_ERROR: ExcCell = ExcCell::new();

/// Module-level state: Comment/PI factories (set by _set_factories), the
/// ElementPath module and copy.deepcopy (looked up on first use).
struct State {
    comment_factory: Option<PyRef>,
    pi_factory: Option<PyRef>,
    element_path: Option<PyRef>,
    deepcopy: Option<PyRef>,
}
static STATE: Mutex<State> = Mutex::new(State { comment_factory: None, pi_factory: None, element_path: None, deepcopy: None });

fn element_type() -> *mut PyTypeObject {
    &raw mut ELEMENT_TYPE
}
fn builder_type() -> *mut PyTypeObject {
    &raw mut BUILDER_TYPE
}
fn parser_type() -> *mut PyTypeObject {
    &raw mut PARSER_TYPE
}

fn none() -> PyRef {
    none::none_ref()
}
fn is_none(o: &PyObject) -> bool {
    o.is(none::none())
}

fn element_path() -> PyResult {
    if let Some(m) = STATE.lock().unwrap().element_path.clone() {
        return Ok(m);
    }
    let m = crate::import::import_absolute("xml.etree.ElementPath")?;
    STATE.lock().unwrap().element_path = Some(m.clone());
    Ok(m)
}

fn deepcopy_func() -> PyResult {
    if let Some(f) = STATE.lock().unwrap().deepcopy.clone() {
        return Ok(f);
    }
    let m = crate::import::import_absolute("copy")?;
    let f = ab::getattr_str(&m, "deepcopy")?;
    STATE.lock().unwrap().deepcopy = Some(f.clone());
    Ok(f)
}

// ======================================================================== Element

/// Number of child slots stored inline in CPython's element layout; used only
/// to report a compatible __sizeof__.
const STATIC_CHILDREN: usize = 4;

pub struct Elem {
    tag: PyRef,
    text: PyRef,
    tail: PyRef,
    attrib: Option<PyRef>,
    children: Vec<PyRef>,
    /// emulated size of the "extra" block: None until attributes or children
    /// exist, then the number of child slots it would hold
    slots: Option<usize>,
}

impl Trav for Elem {
    fn trav(&self, v: VisitFn) -> c_int {
        for p in [&self.tag, &self.text, &self.tail] {
            let r = v(p.as_ptr());
            if r != 0 {
                return r;
            }
        }
        let r = fc::trav_opt(v, &self.attrib);
        if r != 0 {
            return r;
        }
        fc::trav_vec(v, &self.children)
    }
}

#[inline(always)]
fn el(o: &PyObject) -> &mut Elem {
    fc::rs::<Elem>(o)
}

pub fn is_element(o: &PyObject) -> bool {
    o.is_type(element_type()) || crate::types::typeobject::is_subtype(o.type_ptr(), element_type())
}

fn is_element_exact(o: &PyObject) -> bool {
    o.is_type(element_type())
}

fn check_element(o: &PyObject) -> PyResult<()> {
    if !is_element(o) {
        raise!(TypeError, "expected an Element, not {}", fc::tname(o));
    }
    Ok(())
}

fn blank_elem(tag: PyRef, attrib: Option<PyRef>) -> Elem {
    let slots = if attrib.is_some() { Some(STATIC_CHILDREN) } else { None };
    Elem { tag, text: none(), tail: none(), attrib, children: Vec::new(), slots }
}

/// A new exact Element (no copy of `attrib`; the caller hands it over).
fn new_element(tag: PyRef, attrib: Option<PyRef>) -> PyResult {
    fc::robj_new(element_type(), blank_elem(tag, attrib))
}

fn grow_slots(e: &mut Elem, extra: usize) {
    let need = e.children.len() + extra;
    let cur = e.slots.unwrap_or(STATIC_CHILDREN);
    if need > cur {
        e.slots = Some(need + (need >> 3) + if need < 9 { 3 } else { 6 });
    } else if e.slots.is_none() {
        e.slots = Some(STATIC_CHILDREN);
    }
}

fn push_child(parent: &PyObject, child: PyRef) {
    let e = el(parent);
    grow_slots(e, 1);
    e.children.push(child);
}

/// Copy of an attrib mapping merged with keyword arguments, or None if empty.
fn merged_attrib(attrib: Option<&PyObject>, kw: Option<&PyObject>) -> PyResult<Option<PyRef>> {
    let has_kw = kw.is_some_and(|k| dict::dict_len(k) > 0);
    let d = match attrib {
        Some(a) => {
            if !dict::is_anydict(a) {
                raise!(TypeError, "attrib must be dict or frozendict, not {}", fc::tname(a));
            }
            if dict::dict_len(a) == 0 && !has_kw {
                return Ok(None);
            }
            let d = dict::new_dict();
            dict::merge_dict(&d, a, true)?;
            d
        }
        None => {
            if !has_kw {
                return Ok(None);
            }
            dict::new_dict()
        }
    };
    if let Some(k) = kw {
        dict::merge_dict(&d, k, true)?;
    }
    Ok(Some(d))
}

unsafe extern "C" fn element_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    into_c(fc::robj_new(tp, blank_elem(none(), None)))
}

/// Split (tag, attrib={}, **extra) call arguments.
fn split_tag_attrib<'a>(fname: &str, args: &'a [&'a PyObject], kw: Option<&PyObject>) -> PyResult<(&'a PyObject, Option<PyRef>, Option<PyRef>)> {
    if args.is_empty() || args.len() > 2 {
        raise!(TypeError, "{}() takes at most 2 positional arguments ({} given)", fname, args.len());
    }
    let mut attrib = args.get(1).map(|a| a.to_ref());
    let mut rest = None;
    if let Some(k) = kw {
        if dict::dict_len(k) > 0 {
            let k2 = dict::copy(k)?;
            if let Some(a) = dict::pop_item(&k2, &pystr::new_str("attrib"))? {
                if attrib.is_some() {
                    raise!(TypeError, "{}() got multiple values for argument 'attrib'", fname);
                }
                attrib = Some(a);
            }
            rest = Some(k2);
        }
    }
    Ok((args[0], attrib, rest))
}

unsafe extern "C" fn element_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let o = &*o;
        int_into_c((|| -> PyResult<()> {
            let a = fc::targs(args);
            if a.is_empty() {
                raise!(TypeError, "Element() missing required argument 'tag' (pos 1)");
            }
            let (tag, attrib, rest) = split_tag_attrib("Element", a, fc::opt(kw))?;
            let attrib = merged_attrib(attrib.as_deref(), rest.as_deref())?;
            let e = el(o);
            let old_tag = std::mem::replace(&mut e.tag, tag.to_ref());
            if attrib.is_some() && e.slots.is_none() {
                e.slots = Some(STATIC_CHILDREN);
            }
            let old_attrib = std::mem::replace(&mut e.attrib, attrib);
            drop((old_tag, old_attrib));
            Ok(())
        })())
    }
}

unsafe extern "C" fn element_clear_slot(o: *mut PyObject) -> c_int {
    unsafe {
        let e = el(&*o);
        let t = std::mem::replace(&mut e.tag, none());
        let x = std::mem::replace(&mut e.text, none());
        let y = std::mem::replace(&mut e.tail, none());
        let a = e.attrib.take();
        let c = std::mem::take(&mut e.children);
        drop((t, x, y, a, c));
        0
    }
}

unsafe extern "C" fn element_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        into_c((|| -> PyResult {
            let Some(_g) = fc::ReprGuard::enter(o) else {
                raise!(RuntimeError, "reentrant call inside {}.__repr__", fc::raw_tp_name(o));
            };
            let tag = el(o).tag.clone();
            let r = ab::repr_string(&tag)?;
            Ok(pystr::new_str(&format!("<{} {} at {:#x}>", fc::repr_type_name(o), r, o.as_ptr() as usize)))
        })())
    }
}

// -- getters / setters

fn get_tag(o: &PyObject) -> PyResult {
    Ok(el(o).tag.clone())
}
fn get_text(o: &PyObject) -> PyResult {
    Ok(el(o).text.clone())
}
fn get_tail(o: &PyObject) -> PyResult {
    Ok(el(o).tail.clone())
}
fn get_attrib(o: &PyObject) -> PyResult {
    Ok(attrib_dict(o))
}

/// The attrib dict, created on first use.
fn attrib_dict(o: &PyObject) -> PyRef {
    let e = el(o);
    match &e.attrib {
        Some(a) => a.clone(),
        None => {
            let d = dict::new_dict();
            e.attrib = Some(d.clone());
            if e.slots.is_none() {
                e.slots = Some(STATIC_CHILDREN);
            }
            d
        }
    }
}

fn no_delete(v: Option<&PyObject>) -> PyResult<&PyObject> {
    match v {
        Some(v) => Ok(v),
        None => raise!(AttributeError, "can't delete element attribute"),
    }
}

fn set_tag(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = no_delete(v)?;
    let old = std::mem::replace(&mut el(o).tag, v.to_ref());
    drop(old);
    Ok(())
}
fn set_text(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = no_delete(v)?;
    let old = std::mem::replace(&mut el(o).text, v.to_ref());
    drop(old);
    Ok(())
}
fn set_tail(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = no_delete(v)?;
    let old = std::mem::replace(&mut el(o).tail, v.to_ref());
    drop(old);
    Ok(())
}
fn set_attrib(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = no_delete(v)?;
    if !dict::is_dict(v) {
        raise!(TypeError, "attrib must be dict, not {}", fc::tname(v));
    }
    let e = el(o);
    if e.slots.is_none() {
        e.slots = Some(STATIC_CHILDREN);
    }
    let old = std::mem::replace(&mut e.attrib, Some(v.to_ref()));
    drop(old);
    Ok(())
}

// -- sequence protocol

fn nchildren(o: &PyObject) -> usize {
    el(o).children.len()
}

fn child_at(o: &PyObject, i: usize) -> Option<PyRef> {
    el(o).children.get(i).cloned()
}

unsafe extern "C" fn element_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe { nchildren(&*o) as Py_ssize_t }
}

unsafe extern "C" fn element_item(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let o = &*o;
        if i < 0 || i as usize >= nchildren(o) {
            return into_c(Err(err!(IndexError, "child index out of range")));
        }
        child_at(o, i as usize).unwrap().into_raw()
    }
}

fn normalize_index(o: &PyObject, i: isize) -> PyResult<usize> {
    let n = nchildren(o) as isize;
    let j = if i < 0 { i + n } else { i };
    if j < 0 || j >= n {
        raise!(IndexError, "child index out of range");
    }
    Ok(j as usize)
}

fn subscript(o: &PyObject, key: &PyObject) -> PyResult {
    if crate::types::slice::is_slice(key) {
        let (start, stop, step) = crate::types::slice::unpack(key)?;
        let (mut start, mut stop) = (start, stop);
        let n = crate::types::slice::adjust_indices(nchildren(o) as isize, &mut start, &mut stop, step);
        let kids = &el(o).children;
        let mut out = Vec::with_capacity(n.max(0) as usize);
        let mut i = start;
        for _ in 0..n {
            if i < 0 || i as usize >= kids.len() {
                break;
            }
            out.push(kids[i as usize].clone());
            i += step;
        }
        return Ok(list::new_list(out));
    }
    if !ab::has_index(key) {
        raise!(TypeError, "element indices must be integers");
    }
    let i = ab::index_as_ssize(key, Some(crate::types::exceptions::IndexError()))?;
    let j = normalize_index(o, i)?;
    Ok(child_at(o, j).unwrap())
}

fn ass_subscript(o: &PyObject, key: &PyObject, value: Option<&PyObject>) -> PyResult<()> {
    if crate::types::slice::is_slice(key) {
        let seq = match value {
            Some(v) => {
                let items = collect_elements(v)?;
                Some(items)
            }
            None => None,
        };
        let (mut start, mut stop, step) = crate::types::slice::unpack(key)?;
        let len = nchildren(o) as isize;
        let n = crate::types::slice::adjust_indices(len, &mut start, &mut stop, step);
        let e = el(o);
        let removed: Vec<PyRef>;
        match seq {
            None => {
                if n <= 0 {
                    return Ok(());
                }
                if step == 1 {
                    removed = e.children.drain(start as usize..(start + n) as usize).collect();
                } else {
                    let mut idx: Vec<usize> = (0..n).map(|k| (start + k * step) as usize).collect();
                    idx.sort_unstable();
                    let mut out = Vec::with_capacity(idx.len());
                    for &i in idx.iter().rev() {
                        out.push(e.children.remove(i));
                    }
                    removed = out;
                }
            }
            Some(items) => {
                if step == 1 {
                    let lo = start as usize;
                    let hi = (start + n.max(0)) as usize;
                    let hi = hi.max(lo);
                    grow_slots(e, items.len().saturating_sub(hi - lo));
                    removed = e.children.splice(lo..hi, items).collect();
                } else {
                    if items.len() as isize != n {
                        raise!(
                            ValueError,
                            "attempt to assign sequence of size {} to extended slice of size {}",
                            items.len(),
                            n
                        );
                    }
                    let mut out = Vec::with_capacity(items.len());
                    for (k, it) in items.into_iter().enumerate() {
                        let i = (start + k as isize * step) as usize;
                        out.push(std::mem::replace(&mut e.children[i], it));
                    }
                    removed = out;
                }
            }
        }
        drop(removed);
        return Ok(());
    }
    if !ab::has_index(key) {
        raise!(TypeError, "element indices must be integers");
    }
    let i = ab::index_as_ssize(key, Some(crate::types::exceptions::IndexError()))?;
    match value {
        Some(v) => {
            check_element(v)?;
            let j = normalize_index(o, i)?;
            let old = std::mem::replace(&mut el(o).children[j], v.to_ref());
            drop(old);
        }
        None => {
            let j = normalize_index(o, i)?;
            let old = el(o).children.remove(j);
            drop(old);
        }
    }
    Ok(())
}

unsafe extern "C" fn element_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(subscript(&*o, &*k)) }
}

unsafe extern "C" fn element_ass_subscript(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { int_into_c(ass_subscript(&*o, &*k, fc::opt(v))) }
}

unsafe extern "C" fn element_ass_item(o: *mut PyObject, i: Py_ssize_t, v: *mut PyObject) -> c_int {
    unsafe {
        let key = int::new_int(i as i64);
        int_into_c(ass_subscript(&*o, &key, fc::opt(v)))
    }
}

unsafe extern "C" fn element_bool(o: *mut PyObject) -> c_int {
    unsafe {
        let r = crate::modules::warnings::warn(
            crate::types::exceptions::DeprecationWarning(),
            "Testing an element's truth value will always return True in future versions.  \
             Use specific 'len(elem)' or 'elem is not None' test instead.",
            1,
        );
        if r.is_err() {
            return -1;
        }
        (nchildren(&*o) != 0) as c_int
    }
}

// -- methods

fn m_append(o: &PyObject, sub: &PyObject) -> PyResult {
    check_element(sub)?;
    push_child(o, sub.to_ref());
    Ok(none())
}

/// All items of an iterable, each checked to be an Element.  Only a
/// non-iterable argument is reported as "expected sequence"; errors raised
/// while iterating propagate unchanged.
fn collect_elements(v: &PyObject) -> PyResult<Vec<PyRef>> {
    let it = ab::get_iter(v).map_err(|e| {
        if exc_matches(crate::types::exceptions::TypeError()) {
            clear_exc();
            err!(TypeError, "expected sequence, not \"{}\"", fc::tname(v))
        } else {
            e
        }
    })?;
    let mut items = Vec::new();
    while let Some(x) = ab::iter_next(&it)? {
        items.push(x);
    }
    for x in &items {
        check_element(x)?;
    }
    Ok(items)
}

fn m_extend(o: &PyObject, elements: &PyObject) -> PyResult {
    let items = collect_elements(elements)?;
    let e = el(o);
    grow_slots(e, items.len());
    e.children.extend(items);
    Ok(none())
}

fn m_insert(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("insert", args, 2, 2)?;
    let i = ab::index_as_ssize(args[0], None)?;
    check_element(args[1])?;
    let e = el(o);
    let n = e.children.len() as isize;
    let j = if i < 0 { (i + n).max(0) } else { i.min(n) } as usize;
    grow_slots(e, 1);
    e.children.insert(j, args[1].to_ref());
    Ok(none())
}

fn m_remove(o: &PyObject, sub: &PyObject) -> PyResult {
    if !is_element(sub) {
        raise!(TypeError, "remove() argument must be xml.etree.ElementTree.Element, not {}", fc::tname(sub));
    }
    let mut i = 0;
    loop {
        let Some(child) = child_at(o, i) else { break };
        let found = child.is(sub) || ab::rich_compare_bool(&child, sub, Py_EQ)?;
        if found {
            let e = el(o);
            if i < e.children.len() {
                let old = e.children.remove(i);
                drop(old);
            }
            return Ok(none());
        }
        i += 1;
    }
    let r1 = ab::repr_string(sub)?;
    let r2 = ab::repr_string(o)?;
    raise!(ValueError, "{} not in {}", r1, r2)
}

fn m_clear(o: &PyObject) -> PyResult {
    let e = el(o);
    let a = e.attrib.take();
    let c = std::mem::take(&mut e.children);
    let x = std::mem::replace(&mut e.text, none());
    let y = std::mem::replace(&mut e.tail, none());
    e.slots = None;
    drop((a, c, x, y));
    Ok(none())
}

fn m_get(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("get", &["key", "default"], 0, 1);
    let [key, default] = crate::args::parse::<2>(&P, args, kw)?;
    let key = key.unwrap();
    let a = el(o).attrib.clone();
    if let Some(a) = a {
        if let Some(v) = dict::get_item(&a, key)? {
            return Ok(v);
        }
    }
    Ok(default.map_or_else(none, |d| d.to_ref()))
}

fn m_set(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("set", args, 2, 2)?;
    let d = attrib_dict(o);
    dict::set_item(&d, args[0], args[1])?;
    Ok(none())
}

fn m_keys(o: &PyObject) -> PyResult {
    match el(o).attrib.clone() {
        Some(a) => Ok(list::new_list(dict::keys_vec(&a))),
        None => Ok(list::new_list(vec![])),
    }
}

fn m_items(o: &PyObject) -> PyResult {
    match el(o).attrib.clone() {
        Some(a) => Ok(list::new_list(dict::items_vec(&a).into_iter().map(|(k, v)| tuple::tuple2(k, v)).collect())),
        None => Ok(list::new_list(vec![])),
    }
}

fn m_makeelement(_o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("makeelement", args, 2, 2)?;
    let attrib = merged_attrib(Some(args[1]), None)?;
    new_element(args[0].to_ref(), attrib)
}

fn m_copy(o: &PyObject) -> PyResult {
    let (tag, attrib, text, tail, kids) = {
        let e = el(o);
        (e.tag.clone(), e.attrib.clone(), e.text.clone(), e.tail.clone(), e.children.clone())
    };
    let attrib = match attrib {
        Some(a) => Some(dict::copy(&a)?),
        None => None,
    };
    let new = new_element(tag, attrib)?;
    let ne = el(&new);
    ne.text = text;
    ne.tail = tail;
    if !kids.is_empty() {
        grow_slots(ne, kids.len());
        ne.children = kids;
    }
    Ok(new)
}

/// deepcopy of a field value; immutable atoms are shared.
fn deepcopy_value(v: &PyObject, memo: &PyObject) -> PyResult {
    if is_none(v) || pystr::is_str_exact(v) {
        return Ok(v.to_ref());
    }
    let f = deepcopy_func()?;
    ab::call(&f, &[v, memo])
}

fn m_deepcopy(o: &PyObject, memo: &PyObject) -> PyResult {
    if !dict::is_dict(memo) {
        raise!(TypeError, "__deepcopy__() argument must be dict, not {}", fc::tname(memo));
    }
    fc::with_recursion(" in __deepcopy__", || {
        let (tag, attrib) = {
            let e = el(o);
            (e.tag.clone(), e.attrib.clone())
        };
        let tag = deepcopy_value(&tag, memo)?;
        let attrib = match attrib {
            Some(a) => Some(deepcopy_value(&a, memo)?),
            None => None,
        };
        let new = new_element(tag, attrib)?;
        let text = el(o).text.clone();
        let text = deepcopy_value(&text, memo)?;
        el(&new).text = text;
        let tail = el(o).tail.clone();
        let tail = deepcopy_value(&tail, memo)?;
        el(&new).tail = tail;
        let f = deepcopy_func()?;
        let mut i = 0;
        while let Some(child) = child_at(o, i) {
            let c = ab::call(&f, &[&child, memo])?;
            check_element(&c)?;
            push_child(&new, c);
            i += 1;
        }
        let id = int::new_int(o.as_ptr() as usize as i64);
        dict::set_item(memo, &id, &new)?;
        Ok(new)
    })
}

fn m_sizeof(o: &PyObject) -> PyResult {
    let ptr = std::mem::size_of::<usize>();
    let mut size = std::mem::size_of::<PyObject>() + 5 * ptr;
    if let Some(slots) = el(o).slots {
        size += 4 * ptr + STATIC_CHILDREN * ptr;
        if slots > STATIC_CHILDREN {
            size += slots * ptr;
        }
    }
    Ok(int::new_int(size as i64))
}

fn m_getstate(o: &PyObject) -> PyResult {
    let (tag, attrib, text, tail, kids) = {
        let e = el(o);
        (e.tag.clone(), e.attrib.clone(), e.text.clone(), e.tail.clone(), e.children.clone())
    };
    let d = dict::new_dict();
    dict::set_item_str(&d, "tag", &tag)?;
    dict::set_item_str(&d, "attrib", &attrib.unwrap_or_else(dict::new_dict))?;
    dict::set_item_str(&d, "text", &text)?;
    dict::set_item_str(&d, "tail", &tail)?;
    dict::set_item_str(&d, "_children", &list::new_list(kids))?;
    Ok(d)
}

fn m_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    if !dict::is_dict(state) {
        let r = ab::repr_string(state)?;
        raise!(TypeError, "Don't know how to unpickle \"{:.200}\" as an Element", r);
    }
    let Some(tag) = dict::get_item_rs(state, "tag") else {
        raise!(TypeError, "tag may not be NULL");
    };
    let children = match dict::get_item_rs(state, "_children") {
        None => Vec::new(),
        Some(c) if is_none(&c) => Vec::new(),
        Some(c) => {
            if !list::is_list(&c) {
                raise!(TypeError, "'_children' is not a list");
            }
            let v = list::list_to_vec(&c);
            for x in &v {
                check_element(x)?;
            }
            v
        }
    };
    let attrib = match dict::get_item_rs(state, "attrib") {
        None => None,
        Some(a) if is_none(&a) => None,
        Some(a) => {
            if !dict::is_dict(&a) {
                raise!(TypeError, "attrib must be dict, not {}", fc::tname(&a));
            }
            Some(dict::copy(&a)?)
        }
    };
    let text = dict::get_item_rs(state, "text").unwrap_or_else(none);
    let tail = dict::get_item_rs(state, "tail").unwrap_or_else(none);
    let e = el(o);
    let old = (
        std::mem::replace(&mut e.tag, tag),
        std::mem::replace(&mut e.text, text),
        std::mem::replace(&mut e.tail, tail),
        std::mem::replace(&mut e.attrib, attrib),
        std::mem::replace(&mut e.children, children),
    );
    e.slots = None;
    let n = e.children.len();
    if e.attrib.is_some() || n > 0 {
        grow_slots(e, 0);
    }
    drop(old);
    Ok(none())
}

// -- path queries

/// True if `path` is a plain child tag (optionally "{uri}local") that can be
/// matched without ElementPath.
fn simple_tag(path: &PyObject) -> bool {
    if !pystr::is_str_exact(path) {
        return false;
    }
    let s = pystr::to_string_lossy(path);
    if s.is_empty() {
        return false;
    }
    let local = if let Some(rest) = s.strip_prefix('{') {
        match rest.find('}') {
            Some(j) if j > 0 && !rest[..j].contains('*') => &rest[j + 1..],
            _ => return false,
        }
    } else {
        &s[..]
    };
    !local.is_empty()
        && !local
            .chars()
            .any(|c| matches!(c, '/' | '[' | ']' | '@' | '*' | '.' | ':' | '{' | '}' | '(' | ')' | '=' | '\'' | '"' | '!') || c.is_whitespace())
}

fn tag_matches(child: &PyObject, tag: &PyObject) -> PyResult<bool> {
    let ct = el(child).tag.clone();
    if ct.is(tag) {
        return Ok(true);
    }
    if pystr::is_str_exact(&ct) && pystr::is_str_exact(tag) {
        return Ok(pystr::str_eq(&ct, tag));
    }
    ab::rich_compare_bool(&ct, tag, Py_EQ)
}

fn path_call(o: &PyObject, func: &str, args: &[&PyObject]) -> PyResult {
    let m = element_path()?;
    let f = ab::getattr_str(&m, func)?;
    let mut v: Vec<&PyObject> = vec![o];
    v.extend_from_slice(args);
    ab::call(&f, &v)
}

fn m_find(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("find", &["path", "namespaces"], 0, 1);
    let [path, ns] = crate::args::parse::<2>(&P, args, kw)?;
    let path = path.unwrap();
    let ns = ns.unwrap_or(none::none());
    if is_none(ns) && simple_tag(path) {
        let mut i = 0;
        while let Some(c) = child_at(o, i) {
            if is_element(&c) && tag_matches(&c, path)? {
                return Ok(c);
            }
            i += 1;
        }
        return Ok(none());
    }
    path_call(o, "find", &[path, ns])
}

fn m_findtext(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("findtext", &["path", "default", "namespaces"], 0, 1);
    let [path, default, ns] = crate::args::parse::<3>(&P, args, kw)?;
    let path = path.unwrap();
    let default = default.unwrap_or(none::none());
    let ns = ns.unwrap_or(none::none());
    if is_none(ns) && simple_tag(path) {
        let mut i = 0;
        while let Some(c) = child_at(o, i) {
            if is_element(&c) && tag_matches(&c, path)? {
                let t = el(&c).text.clone();
                return Ok(if is_none(&t) { pystr::new_str("") } else { t });
            }
            i += 1;
        }
        return Ok(default.to_ref());
    }
    path_call(o, "findtext", &[path, default, ns])
}

fn m_findall(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("findall", &["path", "namespaces"], 0, 1);
    let [path, ns] = crate::args::parse::<2>(&P, args, kw)?;
    let path = path.unwrap();
    let ns = ns.unwrap_or(none::none());
    if is_none(ns) && simple_tag(path) {
        let mut out = Vec::new();
        let mut i = 0;
        while let Some(c) = child_at(o, i) {
            if is_element(&c) && tag_matches(&c, path)? {
                out.push(c);
            }
            i += 1;
        }
        return Ok(list::new_list(out));
    }
    path_call(o, "findall", &[path, ns])
}

fn m_iterfind(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("iterfind", &["path", "namespaces"], 0, 1);
    let [path, ns] = crate::args::parse::<2>(&P, args, kw)?;
    path_call(o, "iterfind", &[path.unwrap(), ns.unwrap_or(none::none())])
}

// ======================================================================== iterators

struct ElemIter {
    root: Option<PyRef>,
    /// (element, index of the next child to visit)
    stack: Vec<(PyRef, usize)>,
    tag: Option<PyRef>,
    text_mode: bool,
}

impl Trav for ElemIter {
    fn trav(&self, v: VisitFn) -> c_int {
        let r = crate::trav!(v; self.root, self.tag);
        if r != 0 {
            return r;
        }
        for (e, _) in &self.stack {
            let r = v(e.as_ptr());
            if r != 0 {
                return r;
            }
        }
        0
    }
}

fn new_iter(o: &PyObject, tag: Option<PyRef>, text_mode: bool) -> PyResult {
    fc::robj_new(&raw mut ITER_TYPE, ElemIter { root: Some(o.to_ref()), stack: Vec::new(), tag, text_mode })
}

fn m_iter(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("iter", &["tag"], 0, 0);
    let [tag] = crate::args::parse::<1>(&P, args, kw)?;
    let tag = match tag {
        None => None,
        Some(t) if is_none(t) => None,
        Some(t) if pystr::is_str_exact(t) && pystr::str_eq_ascii(t, "*") => None,
        Some(t) => Some(t.to_ref()),
    };
    new_iter(o, tag, false)
}

fn m_itertext(o: &PyObject) -> PyResult {
    new_iter(o, None, true)
}

fn text_ok(tag: &PyObject) -> bool {
    pystr::is_str(tag) || is_none(tag)
}

fn iter_next_impl(it: &mut ElemIter) -> PyResult<Option<PyRef>> {
    if let Some(root) = it.root.take() {
        if it.text_mode {
            if !text_ok(&el(&root).tag) {
                return Ok(None);
            }
            let text = el(&root).text.clone();
            it.stack.push((root, 0));
            if !is_none(&text) && ab::is_true(&text)? {
                return Ok(Some(text));
            }
        } else {
            let matches = match &it.tag {
                None => true,
                Some(t) => tag_matches(&root, t)?,
            };
            it.stack.push((root.clone(), 0));
            if matches {
                return Ok(Some(root));
            }
        }
    }
    loop {
        let Some((top, idx)) = it.stack.last_mut() else { return Ok(None) };
        let child = el(top).children.get(*idx).cloned();
        match child {
            Some(child) => {
                *idx += 1;
                if !is_element(&child) {
                    continue;
                }
                if it.text_mode {
                    if !text_ok(&el(&child).tag) {
                        let tail = el(&child).tail.clone();
                        if !is_none(&tail) && ab::is_true(&tail)? {
                            return Ok(Some(tail));
                        }
                        continue;
                    }
                    let text = el(&child).text.clone();
                    it.stack.push((child, 0));
                    if !is_none(&text) && ab::is_true(&text)? {
                        return Ok(Some(text));
                    }
                } else {
                    let matches = match &it.tag {
                        None => true,
                        Some(t) => tag_matches(&child, t)?,
                    };
                    it.stack.push((child.clone(), 0));
                    if matches {
                        return Ok(Some(child));
                    }
                }
            }
            None => {
                let (done, _) = it.stack.pop().unwrap();
                if it.text_mode && !it.stack.is_empty() {
                    let tail = el(&done).tail.clone();
                    if !is_none(&tail) && ab::is_true(&tail)? {
                        return Ok(Some(tail));
                    }
                }
            }
        }
    }
}

unsafe extern "C" fn iter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = fc::rs::<ElemIter>(&*o);
        match iter_next_impl(it) {
            Ok(Some(v)) => v.into_raw(),
            Ok(None) => {
                let s = std::mem::take(&mut it.stack);
                drop(s);
                std::ptr::null_mut()
            }
            Err(_) => std::ptr::null_mut(),
        }
    }
}

unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

/// Iterator over the direct children (live: re-reads the child list).
struct ChildIter {
    elem: Option<PyRef>,
    index: usize,
}
impl Trav for ChildIter {
    fn trav(&self, v: VisitFn) -> c_int {
        crate::trav!(v; self.elem)
    }
}

unsafe extern "C" fn element_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(fc::robj_new(&raw mut CHILD_ITER_TYPE, ChildIter { elem: Some((*o).to_ref()), index: 0 })) }
}

unsafe extern "C" fn child_iter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = fc::rs::<ChildIter>(&*o);
        let Some(e) = &it.elem else { return std::ptr::null_mut() };
        match el(e).children.get(it.index).cloned() {
            Some(c) => {
                it.index += 1;
                c.into_raw()
            }
            None => {
                let e = it.elem.take();
                drop(e);
                std::ptr::null_mut()
            }
        }
    }
}

// ======================================================================== SubElement / factories

fn f_subelement(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = crate::args::split(args, kw);
    if pos.len() < 2 || pos.len() > 3 {
        raise!(TypeError, "SubElement() takes from 2 to 3 positional arguments but {} were given", pos.len());
    }
    let parent = pos[0];
    if !is_element(parent) {
        raise!(TypeError, "SubElement() argument 1 must be xml.etree.ElementTree.Element, not {}", fc::tname(parent));
    }
    let kwd = if kws.is_empty() {
        None
    } else {
        let d = dict::new_dict();
        for (k, v) in kws {
            dict::set_item(&d, k, v)?;
        }
        Some(d)
    };
    let (tag, attrib, rest) = split_tag_attrib("SubElement", &pos[1..], kwd.as_deref())?;
    let attrib = merged_attrib(attrib.as_deref(), rest.as_deref())?;
    let e = new_element(tag.to_ref(), attrib)?;
    push_child(parent, e.clone());
    Ok(e)
}

fn f_set_factories(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_set_factories", args, 2, 2)?;
    for (a, what) in [(args[0], "Comment"), (args[1], "PI")] {
        if !is_none(a) && !ab::is_callable(a) {
            raise!(TypeError, "{} factory must be callable, not {}", what, fc::tname(a));
        }
    }
    let mut st = STATE.lock().unwrap();
    let opt = |a: &PyObject| if is_none(a) { None } else { Some(a.to_ref()) };
    let old_c = std::mem::replace(&mut st.comment_factory, opt(args[0]));
    let old_p = std::mem::replace(&mut st.pi_factory, opt(args[1]));
    drop(st);
    Ok(tuple::tuple2(old_c.unwrap_or_else(none), old_p.unwrap_or_else(none)))
}

// ======================================================================== TreeBuilder

pub struct Builder {
    root: Option<PyRef>,
    this: Option<PyRef>,
    last: Option<PyRef>,
    /// pending character data pieces
    data: Vec<PyRef>,
    stack: Vec<PyRef>,
    element_factory: Option<PyRef>,
    comment_factory: Option<PyRef>,
    pi_factory: Option<PyRef>,
    insert_comments: bool,
    insert_pis: bool,
}

impl Trav for Builder {
    fn trav(&self, v: VisitFn) -> c_int {
        let r = crate::trav!(v; self.root, self.this, self.last, self.element_factory, self.comment_factory, self.pi_factory);
        if r != 0 {
            return r;
        }
        let r = fc::trav_vec(v, &self.data);
        if r != 0 {
            return r;
        }
        fc::trav_vec(v, &self.stack)
    }
}

#[inline(always)]
fn tb(o: &PyObject) -> &mut Builder {
    fc::rs::<Builder>(o)
}

fn blank_builder() -> Builder {
    Builder {
        root: None,
        this: None,
        last: None,
        data: Vec::new(),
        stack: Vec::new(),
        element_factory: None,
        comment_factory: None,
        pi_factory: None,
        insert_comments: false,
        insert_pis: false,
    }
}

unsafe extern "C" fn builder_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    into_c(fc::robj_new(tp, blank_builder()))
}

unsafe extern "C" fn builder_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let o = &*o;
        int_into_c((|| -> PyResult<()> {
            let a = VcArgs::from_c(args, kw);
            static P: crate::args::Params = crate::args::Params::new(
                "TreeBuilder",
                &["element_factory", "comment_factory", "pi_factory", "insert_comments", "insert_pis"],
                0,
                0,
            )
            .kwonly(1);
            let [ef, cf, pf, ic, ip] = a.parse::<5>(&P)?;
            let opt = |x: Option<PyRef>| x.filter(|v| !is_none(v));
            let (dc, dp) = {
                let st = STATE.lock().unwrap();
                (st.comment_factory.clone(), st.pi_factory.clone())
            };
            let ic = match ic {
                Some(x) => ab::is_true(&x)?,
                None => false,
            };
            let ip = match ip {
                Some(x) => ab::is_true(&x)?,
                None => false,
            };
            let b = tb(o);
            let old = std::mem::replace(b, blank_builder());
            b.element_factory = opt(ef);
            b.comment_factory = opt(cf).or(dc);
            b.pi_factory = opt(pf).or(dp);
            b.insert_comments = ic && b.comment_factory.is_some();
            b.insert_pis = ip && b.pi_factory.is_some();
            drop(old);
            Ok(())
        })())
    }
}

unsafe extern "C" fn builder_clear(o: *mut PyObject) -> c_int {
    unsafe {
        let b = tb(&*o);
        let old = std::mem::replace(b, blank_builder());
        drop(old);
        0
    }
}

/// Attach `child` to `parent` (fast for our Element type, `append` otherwise).
fn add_subelement(parent: &PyObject, child: &PyObject) -> PyResult<()> {
    if is_element_exact(parent) {
        push_child(parent, child.to_ref());
        return Ok(());
    }
    ab::call_method_str(parent, "append", &[child])?;
    Ok(())
}

fn joined_data(pieces: Vec<PyRef>) -> PyResult {
    if pieces.len() == 1 {
        return Ok(pieces.into_iter().next().unwrap());
    }
    let l = list::new_list(pieces);
    crate::types::str_methods::join(&pystr::new_str(""), &l)
}

fn set_text_or_tail(target: &PyObject, data: PyRef, tail: bool) -> PyResult<()> {
    if is_element_exact(target) {
        let e = el(target);
        let slot = if tail { &mut e.tail } else { &mut e.text };
        if is_none(slot) {
            let old = std::mem::replace(slot, data);
            drop(old);
        } else {
            let cur = slot.clone();
            let joined = ab::binary_op(&cur, &data, ab::NbSlot::Add)?;
            let e = el(target);
            let slot = if tail { &mut e.tail } else { &mut e.text };
            let old = std::mem::replace(slot, joined);
            drop(old);
        }
        return Ok(());
    }
    ab::setattr(target, &pystr::new_str(if tail { "tail" } else { "text" }), &data)
}

fn tb_flush(o: &PyObject) -> PyResult<()> {
    let b = tb(o);
    if b.data.is_empty() {
        return Ok(());
    }
    let Some(last) = b.last.clone() else {
        b.data.clear();
        return Ok(());
    };
    let tail = !matches!(&b.this, Some(t) if t.is(&last));
    // perf3: the buffer keeps its capacity (it was reallocated for every text piece)
    let data = if b.data.len() == 1 {
        b.data.pop().unwrap()
    } else {
        let pieces: Vec<PyRef> = b.data.drain(..).collect();
        joined_data(pieces)?
    };
    set_text_or_tail(&last, data, tail)
}

fn tb_data_piece(o: &PyObject, data: PyRef) {
    tb(o).data.push(data);
}

/// Open an element; `attrib` is handed over (already a private dict or None).
fn tb_start(o: &PyObject, tag: PyRef, attrib: Option<PyRef>) -> PyResult {
    tb_flush(o)?;
    let factory = tb(o).element_factory.clone();
    let node = match factory {
        None => new_element(tag, attrib)?,
        Some(f) => {
            let a = attrib.unwrap_or_else(dict::new_dict);
            ab::call(&f, &[&tag, &a])?
        }
    };
    let this = tb(o).this.clone();
    match &this {
        Some(parent) => add_subelement(parent, &node)?,
        None => {
            if tb(o).root.is_none() {
                tb(o).root = Some(node.clone());
            }
        }
    }
    let b = tb(o);
    if let Some(t) = b.this.take() {
        b.stack.push(t);
    }
    b.this = Some(node.clone());
    let old = std::mem::replace(&mut b.last, Some(node.clone()));
    drop(old);
    Ok(node)
}

fn tb_end(o: &PyObject) -> PyResult {
    tb_flush(o)?;
    let b = tb(o);
    let Some(this) = b.this.take() else {
        raise!(IndexError, "pop from empty stack");
    };
    b.this = b.stack.pop();
    let old = std::mem::replace(&mut b.last, Some(this.clone()));
    drop(old);
    Ok(this)
}

fn tb_comment(o: &PyObject, text: &PyObject) -> PyResult {
    let Some(f) = tb(o).comment_factory.clone() else {
        return Ok(text.to_ref());
    };
    let c = ab::call(&f, &[text])?;
    if tb(o).insert_comments {
        if let Some(this) = tb(o).this.clone() {
            tb_flush(o)?;
            add_subelement(&this, &c)?;
            let old = std::mem::replace(&mut tb(o).last, Some(c.clone()));
            drop(old);
        }
    }
    Ok(c)
}

fn tb_pi(o: &PyObject, target: &PyObject, text: &PyObject) -> PyResult {
    let Some(f) = tb(o).pi_factory.clone() else {
        return Ok(none());
    };
    let p = ab::call(&f, &[target, text])?;
    if tb(o).insert_pis {
        if let Some(this) = tb(o).this.clone() {
            tb_flush(o)?;
            add_subelement(&this, &p)?;
            let old = std::mem::replace(&mut tb(o).last, Some(p.clone()));
            drop(old);
        }
    }
    Ok(p)
}

fn tb_close(o: &PyObject) -> PyResult {
    Ok(tb(o).root.clone().unwrap_or_else(none))
}

fn b_start(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("start", args, 2, 2)?;
    if !dict::is_anydict(args[1]) {
        raise!(TypeError, "attrs must be dict, not {}", fc::tname(args[1]));
    }
    let d = dict::new_dict();
    dict::merge_dict(&d, args[1], true)?;
    let attrib = Some(d);
    if tb(o).element_factory.is_none() {
        // a plain Element takes a private copy of the attributes
        let a = attrib.filter(|a| dict::is_dict(a) && dict::dict_len(a) > 0);
        return tb_start(o, args[0].to_ref(), a);
    }
    tb_start(o, args[0].to_ref(), attrib)
}

fn b_end(o: &PyObject, _tag: &PyObject) -> PyResult {
    tb_end(o)
}

fn b_data(o: &PyObject, data: &PyObject) -> PyResult {
    tb_data_piece(o, data.to_ref());
    Ok(none())
}

fn b_comment(o: &PyObject, text: &PyObject) -> PyResult {
    tb_comment(o, text)
}

fn b_pi(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pi", args, 1, 2)?;
    let text = args.get(1).copied().unwrap_or(none::none());
    tb_pi(o, args[0], text)
}

fn b_close(o: &PyObject) -> PyResult {
    tb_close(o)
}

// ======================================================================== XMLParser

const T_START: usize = 0;
const T_END: usize = 1;
const T_DATA: usize = 2;
const T_COMMENT: usize = 3;
const T_PI: usize = 4;
const T_CLOSE: usize = 5;
const T_DOCTYPE: usize = 6;
const T_START_NS: usize = 7;
const T_END_NS: usize = 8;
const TARGET_METHODS: [&str; 9] = ["start", "end", "data", "comment", "pi", "close", "doctype", "start_ns", "end_ns"];

const EV_START: usize = 0;
const EV_END: usize = 1;
const EV_START_NS: usize = 2;
const EV_END_NS: usize = 3;
const EV_COMMENT: usize = 4;
const EV_PI: usize = 5;
const EVENT_NAMES: [&str; 6] = ["start", "end", "start-ns", "end-ns", "comment", "pi"];

/// Character data is delivered to non-builder targets in pieces of at most
/// this many UTF-8 bytes (the buffer size of the expat-based parser).
const TEXT_CHUNK: usize = 8192;

pub struct EtParser {
    engine: Option<Box<Engine>>,
    target: Option<PyRef>,
    entity: Option<PyRef>,
    handlers: [Option<PyRef>; 9],
    /// the target is exactly our TreeBuilder: build directly
    fast: bool,
    names: HashMap<String, PyRef, NameHash>,
    text: String,
    events_append: Option<PyRef>,
    event_names: [Option<PyRef>; 6],
}

impl Trav for EtParser {
    fn trav(&self, v: VisitFn) -> c_int {
        let r = crate::trav!(v; self.target, self.entity, self.events_append);
        if r != 0 {
            return r;
        }
        for h in self.handlers.iter().chain(self.event_names.iter()) {
            let r = fc::trav_opt(v, h);
            if r != 0 {
                return r;
            }
        }
        0
    }
}

#[inline(always)]
fn xp(o: &PyObject) -> &mut EtParser {
    fc::rs::<EtParser>(o)
}

fn blank_parser() -> EtParser {
    EtParser {
        engine: None,
        target: None,
        entity: None,
        handlers: Default::default(),
        fast: false,
        names: HashMap::default(),
        text: String::new(),
        events_append: None,
        event_names: Default::default(),
    }
}

unsafe extern "C" fn parser_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    into_c(fc::robj_new(tp, blank_parser()))
}

unsafe extern "C" fn parser_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let o = &*o;
        int_into_c((|| -> PyResult<()> {
            let a = VcArgs::from_c(args, kw);
            static P: crate::args::Params = crate::args::Params::new("XMLParser", &["target", "encoding"], 0, 0).kwonly(2);
            let [target, encoding] = a.parse::<2>(&P)?;
            let encoding = match encoding {
                None => None,
                Some(e) if is_none(&e) => None,
                Some(e) if pystr::is_str(&e) => Some(pystr::to_string_lossy(&e)),
                Some(e) => raise!(TypeError, "XMLParser() argument 'encoding' must be str or None, not {}", fc::tname(&e)),
            };
            let target = match target {
                Some(t) if !is_none(&t) => t,
                _ => fc::robj_new(builder_type(), {
                    let mut b = blank_builder();
                    let st = STATE.lock().unwrap();
                    b.comment_factory = st.comment_factory.clone();
                    b.pi_factory = st.pi_factory.clone();
                    b
                })?,
            };
            let fast = target.is_type(builder_type());
            let mut handlers: [Option<PyRef>; 9] = Default::default();
            if !fast {
                for (i, m) in TARGET_METHODS.iter().enumerate() {
                    handlers[i] = ab::getattr_opt(&target, &pystr::new_str(m))?;
                }
            }
            let mut eng = Engine::new(Some("}".to_string()), encoding);
            eng.reparse_deferral = true;
            let p = xp(o);
            let old = std::mem::replace(p, blank_parser());
            p.engine = Some(Box::new(eng));
            p.target = Some(target);
            p.entity = Some(dict::new_dict());
            p.handlers = handlers;
            p.fast = fast;
            drop(old);
            Ok(())
        })())
    }
}

unsafe extern "C" fn parser_clear(o: *mut PyObject) -> c_int {
    unsafe {
        let p = xp(&*o);
        let t = p.target.take();
        let e = p.entity.take();
        let a = p.events_append.take();
        let hs: Vec<Option<PyRef>> = p.handlers.iter_mut().map(|h| h.take()).collect();
        let ns: Vec<Option<PyRef>> = p.event_names.iter_mut().map(|h| h.take()).collect();
        let names = std::mem::take(&mut p.names);
        drop((t, e, a, hs, ns, names));
        0
    }
}

fn check_init(o: &PyObject) -> PyResult<()> {
    if xp(o).engine.is_none() {
        raise!(ValueError, "XMLParser.__init__() wasn't called");
    }
    Ok(())
}

pub fn parse_error_type() -> PyResult<*mut PyTypeObject> {
    PARSE_ERROR.get_or_create("xml.etree.ElementTree", "ParseError", crate::types::exceptions::SyntaxError())
}

fn parse_error(code: u32, line: u64, col: u64, msg: Option<String>) -> PyErr {
    let r = (|| -> PyResult<PyRef> {
        let msg = msg.unwrap_or_else(|| format!("{}: line {}, column {}", px::error_string(code).unwrap_or("unknown error"), line, col));
        let tp = parse_error_type()?;
        let e = ab::call(type_obj(tp), &[&pystr::new_str(&msg)])?;
        ab::setattr(&e, &pystr::new_str("code"), &int::new_int(code as i64))?;
        let pos = tuple::tuple2(int::new_int(line as i64), int::new_int(col as i64));
        ab::setattr(&e, &pystr::new_str("position"), &pos)?;
        Ok(e)
    })();
    match r {
        Ok(e) => {
            set_exc(e);
            PyErr
        }
        Err(e) => e,
    }
}

/// expat reports "{uri}local" as "uri}local": restore the brace (cached).
fn fixname(o: &PyObject, raw: &str) -> PyRef {
    let p = xp(o);
    if let Some(v) = p.names.get(raw) {
        return v.clone();
    }
    let v = if raw.contains('}') { pystr::new_str(&format!("{{{}", raw)) } else { pystr::new_str(raw) };
    p.names.insert(raw.to_string(), v.clone());
    v
}

fn emit_event(o: &PyObject, which: usize, value: &PyObject) -> PyResult<()> {
    let p = xp(o);
    let (Some(append), Some(name)) = (p.events_append.clone(), p.event_names[which].clone()) else {
        return Ok(());
    };
    ab::call(&append, &[&tuple::tuple2(name, value.to_ref())])?;
    Ok(())
}

fn wants_event(o: &PyObject, which: usize) -> bool {
    xp(o).event_names[which].is_some()
}

fn flush_text(o: &PyObject) -> PyResult<()> {
    if xp(o).text.is_empty() {
        return Ok(());
    }
    let text = std::mem::take(&mut xp(o).text);
    deliver_data(o, pystr::new_str(&text))
}

fn deliver_data(o: &PyObject, data: PyRef) -> PyResult<()> {
    let p = xp(o);
    if p.fast {
        let t = p.target.clone().unwrap();
        tb_data_piece(&t, data);
        return Ok(());
    }
    if let Some(h) = p.handlers[T_DATA].clone() {
        ab::call(&h, &[&data])?;
    }
    Ok(())
}

fn char_data(o: &PyObject, t: &str) -> PyResult<()> {
    let p = xp(o);
    if p.fast {
        p.text.push_str(t);
        return Ok(());
    }
    if p.handlers[T_DATA].is_none() {
        return Ok(());
    }
    if p.text.len() + t.len() > TEXT_CHUNK {
        flush_text(o)?;
    }
    if t.len() > TEXT_CHUNK {
        return deliver_data(o, pystr::new_str(t));
    }
    xp(o).text.push_str(t);
    Ok(())
}

fn entity_ref(o: &PyObject, name: &str, line: u64, col: u64) -> PyResult<()> {
    let p = xp(o);
    if !p.fast && p.handlers[T_DATA].is_none() {
        return Ok(());
    }
    let entity = p.entity.clone();
    let val = match entity {
        Some(d) => dict::get_item(&d, &pystr::new_str(name))?,
        None => None,
    };
    match val {
        Some(v) => {
            flush_text(o)?;
            deliver_data(o, v)
        }
        None => Err(parse_error(11, line, col, Some(format!("undefined entity &{};: line {}, column {}", name, line, col)))),
    }
}

fn ev_start(o: &PyObject, name: &str, attrs: Vec<(String, String)>) -> PyResult<()> {
    let tag = fixname(o, name);
    let p = xp(o);
    if p.fast {
        let attrib = if attrs.is_empty() {
            None
        } else {
            let d = dict::new_dict_presized(attrs.len());
            for (k, v) in &attrs {
                let key = fixname(o, k);
                dict::set_item(&d, &key, &pystr::new_str(v))?;
            }
            Some(d)
        };
        let t = xp(o).target.clone().unwrap();
        let node = tb_start(&t, tag, attrib)?;
        return emit_event(o, EV_START, &node);
    }
    let h = p.handlers[T_START].clone();
    let want = wants_event(o, EV_START);
    if h.is_none() && !want {
        return Ok(());
    }
    let d = dict::new_dict_presized(attrs.len());
    for (k, v) in &attrs {
        let key = fixname(o, k);
        dict::set_item(&d, &key, &pystr::new_str(v))?;
    }
    let res = match h {
        Some(h) => ab::call(&h, &[&tag, &d])?,
        None => none(),
    };
    emit_event(o, EV_START, &res)
}

fn ev_end(o: &PyObject, name: &str) -> PyResult<()> {
    let p = xp(o);
    if p.fast {
        let t = p.target.clone().unwrap();
        let node = tb_end(&t)?;
        return emit_event(o, EV_END, &node);
    }
    let h = p.handlers[T_END].clone();
    let want = wants_event(o, EV_END);
    if h.is_none() && !want {
        return Ok(());
    }
    let tag = fixname(o, name);
    let res = match h {
        Some(h) => ab::call(&h, &[&tag])?,
        None => none(),
    };
    emit_event(o, EV_END, &res)
}

fn opt_s(s: &Option<String>) -> PyRef {
    match s {
        Some(s) => pystr::new_str(s),
        None => none(),
    }
}

fn dispatch(o: &PyObject, events: Vec<Event>) -> PyResult<Option<String>> {
    for ev in events {
        if !matches!(ev.ev, Ev::CharData(_)) {
            flush_text(o)?;
        }
        match ev.ev {
            Ev::CharData(t) => char_data(o, &t)?,
            Ev::StartElement { name, attrs, .. } => ev_start(o, &name, attrs)?,
            Ev::EndElement { name } => ev_end(o, &name)?,
            Ev::Error(code) => return Err(parse_error(code, ev.line, ev.col, None)),
            Ev::NeedEncoding(name) => return Ok(Some(name)),
            Ev::StartNs { prefix, uri } => {
                let h = xp(o).handlers[T_START_NS].clone();
                let want = wants_event(o, EV_START_NS);
                if h.is_none() && !want {
                    continue;
                }
                let prefix = pystr::new_str(prefix.as_deref().unwrap_or(""));
                let uri = pystr::new_str(uri.as_deref().unwrap_or(""));
                let res = match h {
                    Some(h) => ab::call(&h, &[&prefix, &uri])?,
                    None => tuple::tuple2(prefix, uri),
                };
                emit_event(o, EV_START_NS, &res)?;
            }
            Ev::EndNs { prefix } => {
                let h = xp(o).handlers[T_END_NS].clone();
                let want = wants_event(o, EV_END_NS);
                if h.is_none() && !want {
                    continue;
                }
                let res = match h {
                    Some(h) => ab::call(&h, &[&pystr::new_str(prefix.as_deref().unwrap_or(""))])?,
                    None => none(),
                };
                emit_event(o, EV_END_NS, &res)?;
            }
            Ev::Comment(text) => {
                let p = xp(o);
                let text = pystr::new_str(&text);
                let res = if p.fast {
                    let t = p.target.clone().unwrap();
                    tb_comment(&t, &text)?
                } else if let Some(h) = p.handlers[T_COMMENT].clone() {
                    ab::call(&h, &[&text])?
                } else {
                    continue;
                };
                emit_event(o, EV_COMMENT, &res)?;
            }
            Ev::Pi { target, data } => {
                let p = xp(o);
                let tgt = pystr::new_str(&target);
                let data = pystr::new_str(&data);
                let res = if p.fast {
                    let t = p.target.clone().unwrap();
                    tb_pi(&t, &tgt, &data)?
                } else if let Some(h) = p.handlers[T_PI].clone() {
                    ab::call(&h, &[&tgt, &data])?
                } else {
                    continue;
                };
                emit_event(o, EV_PI, &res)?;
            }
            Ev::StartDoctype(boxed) => {
            let crate::modules::pyexpat::engine::StartDoctypeEv { name, sysid, pubid, .. } = *boxed;
                let h = xp(o).handlers[T_DOCTYPE].clone();
                if let Some(h) = h {
                    ab::call(&h, &[&pystr::new_str(&name), &opt_s(&pubid), &opt_s(&sysid)])?;
                } else if !o.is_type(parser_type()) && ab::getattr_opt(o, &pystr::new_str("doctype"))?.is_some() {
                    crate::modules::warnings::warn(
                        crate::types::exceptions::RuntimeWarning(),
                        "The doctype() method of XMLParser is ignored.  Define doctype() method on the TreeBuilder target.",
                        1,
                    )?;
                }
            }
            Ev::Default(t) => {
                if let Some(name) = t.strip_prefix('&').and_then(|r| r.strip_suffix(';')) {
                    entity_ref(o, name, ev.line, ev.col)?;
                }
            }
            Ev::SkippedEntity { name, is_pe } => {
                if !is_pe {
                    entity_ref(o, &name, ev.line, ev.col)?;
                }
            }
            Ev::ExternalEntityRef(r) => {
                if let Some(c) = &r.context {
                    entity_ref(o, c, ev.line, ev.col)?;
                }
            }
            _ => {}
        }
    }
    Ok(None)
}

fn parser_feed_bytes(o: &PyObject, data: &[u8], final_: bool) -> PyResult<()> {
    let mut events = xp(o).engine.as_deref_mut().unwrap().feed(data, final_);
    loop {
        let need = dispatch(o, events)?;
        let Some(name) = need else { break };
        match px::encoding_table(&name) {
            Ok(t) => events = xp(o).engine.as_deref_mut().unwrap().set_encoding_table(t),
            Err(e) => {
                xp(o).engine.as_deref_mut().unwrap().encoding_failed();
                return Err(e);
            }
        }
    }
    flush_text(o)
}

fn feed_obj(o: &PyObject, data: &PyObject, final_: bool) -> PyResult<()> {
    if pystr::is_str(data) {
        let eng = xp(o).engine.as_deref_mut().unwrap();
        if !eng.is_started() {
            eng.force_encoding("utf-8".to_string());
        }
        if let Some(s) = pystr::to_string_strict(data) {
            return parser_feed_bytes(o, s.as_bytes(), final_);
        }
        let b = ab::call_method_str(data, "encode", &[&pystr::new_str("utf-8"), &pystr::new_str("surrogatepass")])?;
        let bytes = crate::buffer::buffer_bytes(&b).unwrap_or_default();
        return parser_feed_bytes(o, &bytes, final_);
    }
    let Some(bytes) = crate::buffer::buffer_bytes(data) else {
        raise!(TypeError, "a bytes-like object is required, not '{}'", fc::tname(data));
    };
    parser_feed_bytes(o, &bytes, final_)
}

fn p_feed(o: &PyObject, data: &PyObject) -> PyResult {
    check_init(o)?;
    feed_obj(o, data, false)?;
    Ok(none())
}

fn finish(o: &PyObject) -> PyResult {
    parser_feed_bytes(o, b"", true)?;
    let p = xp(o);
    if p.fast {
        let t = p.target.clone().unwrap();
        return tb_close(&t);
    }
    match p.handlers[T_CLOSE].clone() {
        Some(h) => ab::call0(&h),
        None => Ok(none()),
    }
}

fn p_close(o: &PyObject) -> PyResult {
    check_init(o)?;
    finish(o)
}

fn p_flush(o: &PyObject) -> PyResult {
    check_init(o)?;
    let was = {
        let eng = xp(o).engine.as_deref_mut().unwrap();
        let was = eng.reparse_deferral;
        eng.reparse_deferral = false;
        was
    };
    let r = parser_feed_bytes(o, b"", false);
    if let Some(eng) = xp(o).engine.as_deref_mut() {
        eng.reparse_deferral = was;
    }
    r?;
    Ok(none())
}

fn p_parse_whole(o: &PyObject, file: &PyObject) -> PyResult {
    check_init(o)?;
    let read = ab::getattr_str(file, "read")?;
    let n = int::new_int(64 * 1024);
    loop {
        let chunk = ab::call(&read, &[&n])?;
        if pystr::is_str(&chunk) {
            if pystr::str_len(&chunk) == 0 {
                break;
            }
            feed_obj(o, &chunk, false)?;
            continue;
        }
        if !(crate::types::bytes::is_bytes(&chunk) || crate::types::bytearray::is_bytearray(&chunk)) {
            raise!(TypeError, "read() did not return a bytes object (type={:.400})", fc::tname(&chunk));
        }
        let b = crate::buffer::buffer_bytes(&chunk).unwrap_or_default();
        if b.is_empty() {
            break;
        }
        parser_feed_bytes(o, &b, false)?;
    }
    finish(o)
}

fn p_setevents(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_setevents", args, 1, 2)?;
    check_init(o)?;
    let queue = args[0];
    let mut names: [Option<PyRef>; 6] = Default::default();
    if let Some(evs) = args.get(1).filter(|e| !is_none(e)) {
        let items = ab::collect_iterable(evs)?;
        for it in items {
            let s = if pystr::is_str(&it) { Some(pystr::to_string_lossy(&it)) } else { None };
            match s.as_deref().and_then(|s| EVENT_NAMES.iter().position(|n| *n == s)) {
                Some(i) => names[i] = Some(it.clone()),
                None => {
                    let r = ab::repr_string(&it)?;
                    raise!(ValueError, "unknown event {}", r);
                }
            }
        }
    }
    let append = ab::getattr_str(queue, "append")?;
    let p = xp(o);
    let old_a = std::mem::replace(&mut p.events_append, Some(append));
    let old_n = std::mem::replace(&mut p.event_names, names);
    drop((old_a, old_n));
    Ok(none())
}

fn get_target(o: &PyObject) -> PyResult {
    Ok(xp(o).target.clone().unwrap_or_else(none))
}
fn get_entity(o: &PyObject) -> PyResult {
    Ok(xp(o).entity.clone().unwrap_or_else(none))
}
fn get_version(_o: &PyObject) -> PyResult {
    Ok(pystr::new_str("Expat 2.7.3"))
}

// ======================================================================== module

fn leak<T>(v: T) -> *mut T {
    Box::into_raw(Box::new(v))
}

fn init_types() -> PyResult<()> {
    unsafe {
        let t = element_type();
        if (*t).tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        fc::init_robj_type::<Elem>(t, c"xml.etree.ElementTree.Element", Py_TPFLAGS_BASETYPE);
        (*t).tp_new = Some(element_new);
        (*t).tp_init = Some(element_init);
        (*t).tp_clear = Some(element_clear_slot);
        (*t).tp_repr = Some(element_repr);
        (*t).tp_iter = Some(element_iter);
        (*t).tp_setattro = Some(crate::types::typeobject::object_generic_setattro);
        let mut sq: PySequenceMethods = std::mem::zeroed();
        sq.sq_length = Some(element_len);
        sq.sq_item = Some(element_item);
        sq.sq_ass_item = Some(element_ass_item);
        (*t).tp_as_sequence = leak(sq);
        let mut mp: PyMappingMethods = std::mem::zeroed();
        mp.mp_length = Some(element_len);
        mp.mp_subscript = Some(element_subscript);
        mp.mp_ass_subscript = Some(element_ass_subscript);
        (*t).tp_as_mapping = leak(mp);
        let mut nb: PyNumberMethods = std::mem::zeroed();
        nb.nb_bool = Some(element_bool);
        (*t).tp_as_number = leak(nb);
        (*t).tp_methods = mdefs![
            ("clear", NOARGS, m_clear, "Remove all subelements, attributes, text and tail."),
            ("get", FASTKW, m_get, "get($self, /, key, default=None)\n--\n\nReturn an attribute value or default."),
            ("find", FASTKW, m_find, "find($self, /, path, namespaces=None)\n--\n\nReturn the first matching subelement, or None."),
            ("findtext", FASTKW, m_findtext, "findtext($self, /, path, default=None, namespaces=None)\n--\n\nReturn the text of the first matching subelement."),
            ("findall", FASTKW, m_findall, "findall($self, /, path, namespaces=None)\n--\n\nReturn all matching subelements."),
            ("append", O, m_append, "append($self, subelement, /)\n--\n\nAdd a subelement at the end."),
            ("extend", O, m_extend, "extend($self, elements, /)\n--\n\nAppend subelements from a sequence."),
            ("insert", FAST, m_insert, "insert($self, index, subelement, /)\n--\n\nInsert a subelement at the given position."),
            ("remove", O, m_remove, "remove($self, subelement, /)\n--\n\nRemove the first matching subelement."),
            ("iter", FASTKW, m_iter, "iter($self, /, tag=None)\n--\n\nIterate over this element and its descendants."),
            ("itertext", NOARGS, m_itertext, "itertext($self, /)\n--\n\nIterate over the text of this subtree."),
            ("iterfind", FASTKW, m_iterfind, "iterfind($self, /, path, namespaces=None)\n--\n\nIterate over matching subelements."),
            ("items", NOARGS, m_items, "items($self, /)\n--\n\nThe attributes as (name, value) pairs."),
            ("keys", NOARGS, m_keys, "keys($self, /)\n--\n\nThe attribute names."),
            ("makeelement", FAST, m_makeelement, "makeelement($self, tag, attrib, /)\n--\n\nCreate a new element."),
            ("set", FAST, m_set, "set($self, key, value, /)\n--\n\nSet an attribute."),
            ("__copy__", NOARGS, m_copy, "__copy__($self, /)\n--\n\n"),
            ("__deepcopy__", O, m_deepcopy, "__deepcopy__($self, memo, /)\n--\n\n"),
            ("__sizeof__", NOARGS, m_sizeof, "__sizeof__($self, /)\n--\n\n"),
            ("__getstate__", NOARGS, m_getstate, "__getstate__($self, /)\n--\n\n"),
            ("__setstate__", O, m_setstate, "__setstate__($self, state, /)\n--\n\n"),
        ];
        (*t).tp_getset = crate::getset_table![
            ("tag", get_tag, set_tag; "A string identifying what kind of data this element represents"),
            ("text", get_text, set_text; "A string of text directly after the start tag, or None"),
            ("tail", get_tail, set_tail; "A string of text directly after the end tag, or None"),
            ("attrib", get_attrib, set_attrib; "A dictionary containing the element's attributes"),
        ];
        crate::types::typeobject::type_ready(t)?;

        let it = &raw mut ITER_TYPE;
        fc::init_robj_type::<ElemIter>(it, c"_elementtree._element_iterator", Py_TPFLAGS_DISALLOW_INSTANTIATION);
        (*it).tp_iter = Some(self_iter);
        (*it).tp_iternext = Some(iter_next);
        crate::types::typeobject::type_ready(it)?;

        let ci = &raw mut CHILD_ITER_TYPE;
        fc::init_robj_type::<ChildIter>(ci, c"_elementtree._child_iterator", Py_TPFLAGS_DISALLOW_INSTANTIATION);
        (*ci).tp_iter = Some(self_iter);
        (*ci).tp_iternext = Some(child_iter_next);
        crate::types::typeobject::type_ready(ci)?;

        let b = builder_type();
        fc::init_robj_type::<Builder>(b, c"xml.etree.ElementTree.TreeBuilder", Py_TPFLAGS_BASETYPE);
        (*b).tp_new = Some(builder_new);
        (*b).tp_init = Some(builder_init);
        (*b).tp_clear = Some(builder_clear);
        (*b).tp_setattro = Some(crate::types::typeobject::object_generic_setattro);
        (*b).tp_methods = mdefs![
            ("data", O, b_data, "data($self, data, /)\n--\n\n"),
            ("start", FAST, b_start, "start($self, tag, attrs, /)\n--\n\n"),
            ("end", O, b_end, "end($self, tag, /)\n--\n\n"),
            ("comment", O, b_comment, "comment($self, text, /)\n--\n\n"),
            ("pi", FAST, b_pi, "pi($self, target, text=None, /)\n--\n\n"),
            ("close", NOARGS, b_close, "close($self, /)\n--\n\n"),
        ];
        crate::types::typeobject::type_ready(b)?;

        let p = parser_type();
        fc::init_robj_type::<EtParser>(p, c"xml.etree.ElementTree.XMLParser", Py_TPFLAGS_BASETYPE);
        (*p).tp_new = Some(parser_new);
        (*p).tp_init = Some(parser_init);
        (*p).tp_clear = Some(parser_clear);
        (*p).tp_setattro = Some(crate::types::typeobject::object_generic_setattro);
        (*p).tp_methods = mdefs![
            ("feed", O, p_feed, "feed($self, data, /)\n--\n\n"),
            ("close", NOARGS, p_close, "close($self, /)\n--\n\n"),
            ("flush", NOARGS, p_flush, "flush($self, /)\n--\n\n"),
            ("_parse_whole", O, p_parse_whole, "_parse_whole($self, file, /)\n--\n\n"),
            ("_setevents", FAST, p_setevents, "_setevents($self, events_queue, events_to_report=None, /)\n--\n\n"),
        ];
        (*p).tp_getset = crate::getset_table![("target", get_target), ("entity", get_entity), ("version", get_version)];
        crate::types::typeobject::type_ready(p)?;
    }
    Ok(())
}

pub fn init() -> PyResult {
    init_types()?;
    {
        // a fresh import starts from a clean module state
        let mut st = STATE.lock().unwrap();
        let old = (st.comment_factory.take(), st.pi_factory.take(), st.element_path.take(), st.deepcopy.take());
        drop(st);
        drop(old);
    }
    let m = crate::types::module::new_module("_elementtree")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        mdefs![("_set_factories", FAST, f_set_factories, "_set_factories($module, comment_factory, pi_factory, /)\n--\n\nChange the factories used to create comments and processing instructions.\n\nFor internal use only.")],
        Some(&m),
    )?;
    let sub = crate::builtins::add_methods_to_dict(
        &d,
        mdefs![("SubElement", FASTKW, f_subelement, "SubElement(parent, tag, /, attrib={}, **extra)\n--\n\n")],
        Some(&m),
    );
    sub?;
    dict::set_item_str(&d, "Element", type_obj(element_type()))?;
    dict::set_item_str(&d, "TreeBuilder", type_obj(builder_type()))?;
    dict::set_item_str(&d, "XMLParser", type_obj(parser_type()))?;
    dict::set_item_str(&d, "ParseError", type_obj(parse_error_type()?))?;
    let _ = type_ref;
    let _ = bool_::new_bool;
    Ok(m)
}
