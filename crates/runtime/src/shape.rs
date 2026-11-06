//! Inline instance attributes ("shapes"; perf-runtime / perf2 workstreams).
//!
//! Instances of ordinary Python classes (heap types with a managed `__dict__` and a
//! fixed-size layout) do not get a dict when they are created. Their attribute values
//! live in value slots allocated together with the object, right after the type's
//! `tp_basicsize` (rounded up to 8):
//!
//! ```text
//!   [managed dict word][weakref][gc next][gc prev] PyObject ... [v0 v1 v2 ...]
//! ```
//!
//! The attribute *names* live in a shared, immutable [`Shape`]: an ordered key list.
//! Each type owns a transition tree of shapes (root = no attributes; adding attribute
//! `n` to an object of shape `S` moves it to the child `S + n`), so all instances that
//! got their attributes in the same order share one shape, like HotSpot/V8 hidden
//! classes. A shape is the guard JIT caches can use: same shape => same index.
//!
//! The managed-dict word in the pre-header encodes the state:
//! * `0`               no dict and no values (allocation did not go through
//!                     `generic_alloc`, or the dict was deleted): dict mode;
//! * `shape | 1`       the attributes are inline, described by `shape` (tag bit 0);
//! * a dict pointer    a real dict was materialised; it is authoritative from then on
//!                     (requested through `__dict__`/`vars()`/C API, too many
//!                     attributes, unusual deletions, non-str names, `__class__`
//!                     assignment).
//!
//! The number of value slots an instance has room for is decided at allocation (the
//! type's learned capacity, rounded up to what the allocator's size class holds) and
//! kept in the high byte of `ob_flags` (CPython only uses its low bits).
//!
//! Every path that hands out the dict slot (`typeobject::dict_slot`,
//! `_PyObject_GetDictPtr`, `PyObject_GenericGetDict`, ...) materialises first, so code
//! that only knows dicts keeps working unchanged.
//!
//! Concurrency (no GIL): writers take the object's `ob_mutex` once a second thread
//! exists; readers are lock-free (acquire loads of the word and the value). Values
//! replaced or removed while other threads may be reading are released through
//! [`crate::qsbr`] (deferred until every thread passed a quiescent point), so a racing
//! reader never increfs a freed object. Shapes themselves are immutable and freed only
//! with their type (instances keep their type alive; `__class__` assignment
//! materialises the dict first).

use crate::ffi::*;
use crate::object::*;
use crate::types::dict;
use crate::types::str as pystr;
use std::ffi::{c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering::*};

/// Maximum number of inline attributes; more go to a materialised dict.
pub const MAX_INLINE: u32 = 30;
/// Maximum number of shapes per type (polymorphic insertion orders); beyond this new
/// transitions materialise the dict ("megamorphic").
const MAX_SHAPES: u32 = 256;
/// Capacity of the values area for types without `__static_attributes__` hints.
const DEFAULT_CAP: u32 = 4;
/// `ob_flags` bits holding the inline capacity of an instance.
const CAP_SHIFT: u32 = 8;

/// An ordered list of attribute names (interned, immortal strs).
#[repr(C)]
pub struct Shape {
    /// Globally unique, never reused (0 is never a valid id).
    pub id: u32,
    pub len: u32,
    /// `len` interned names, in insertion order.
    pub keys: *const *mut PyObject,
    pub parent: *const Shape,
    pub tree: *const ShapeTree,
    /// Byte offset of the first value slot from the object start (the owning type's
    /// rounded `tp_basicsize`; every shape of a tree belongs to one type).
    pub voff: u32,
    /// Children, as an immutable list replaced on insertion (readers are lock-free).
    trans: AtomicPtr<TransList>,
}
unsafe impl Sync for Shape {}
unsafe impl Send for Shape {}

struct TransList {
    items: Vec<(usize, *const Shape)>,
}

/// Per-type shape tree, stored in `PyHeapTypeObject.ht_cached_keys`.
pub struct ShapeTree {
    root: *const Shape,
    /// Capacity of the values area of new instances (learned: grows when instances
    /// overflow it, up to MAX_INLINE).
    cap: AtomicU32,
    nshapes: AtomicU32,
    inner: Mutex<TreeInner>,
}
struct TreeInner {
    shapes: Vec<*mut Shape>,
    keys: Vec<Box<[*mut PyObject]>>,
    retired: Vec<*mut TransList>,
}
unsafe impl Send for TreeInner {}

static NEXT_SHAPE_ID: AtomicU32 = AtomicU32::new(1);

impl Shape {
    #[inline(always)]
    pub fn keys(&self) -> &[*mut PyObject] {
        if self.len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(self.keys, self.len as usize) } }
    }
    /// Index of interned name `k` (pointer comparison).
    #[inline(always)]
    pub fn index_of(&self, k: *mut PyObject) -> Option<usize> {
        self.keys().iter().position(|&x| x == k)
    }
    #[inline]
    fn find_transition(&self, k: *mut PyObject) -> Option<*const Shape> {
        let t = self.trans.load(Acquire);
        if t.is_null() {
            return None;
        }
        unsafe { (*t).items.iter().find(|&&(n, _)| n == k as usize).map(|&(_, s)| s) }
    }
    /// The shape with `k` appended (created on first use). None if the tree is full.
    fn add(&self, k: *mut PyObject) -> Option<*const Shape> {
        if let Some(s) = self.find_transition(k) {
            return Some(s);
        }
        let tree = unsafe { &*self.tree };
        let mut g = tree.inner.lock().unwrap();
        if let Some(s) = self.find_transition(k) {
            return Some(s);
        }
        if tree.nshapes.load(Relaxed) >= MAX_SHAPES || self.len >= MAX_INLINE {
            return None;
        }
        let mut keys: Vec<*mut PyObject> = self.keys().to_vec();
        keys.push(k);
        let keys = keys.into_boxed_slice();
        let child = Box::into_raw(Box::new(Shape {
            id: NEXT_SHAPE_ID.fetch_add(1, Relaxed),
            len: self.len + 1,
            keys: keys.as_ptr(),
            parent: self as *const Shape,
            tree: self.tree,
            voff: self.voff,
            trans: AtomicPtr::new(std::ptr::null_mut()),
        }));
        g.keys.push(keys);
        g.shapes.push(child);
        tree.nshapes.fetch_add(1, Relaxed);
        let old = self.trans.load(Acquire);
        let mut items = if old.is_null() { Vec::with_capacity(1) } else { unsafe { (*old).items.clone() } };
        items.push((k as usize, child as *const Shape));
        let nl = Box::into_raw(Box::new(TransList { items }));
        self.trans.store(nl, Release);
        if !old.is_null() {
            g.retired.push(old); // lock-free readers may still be scanning it
        }
        Some(child)
    }
}

impl Drop for ShapeTree {
    fn drop(&mut self) {
        let g = self.inner.get_mut().unwrap();
        unsafe {
            for &s in g.shapes.iter() {
                let t = (*s).trans.load(Relaxed);
                if !t.is_null() {
                    drop(Box::from_raw(t));
                }
                drop(Box::from_raw(s));
            }
            for &t in g.retired.iter() {
                drop(Box::from_raw(t));
            }
        }
    }
}

// ------------------------------------------------------------------ types

#[inline(always)]
pub fn tree_of(tp: *mut PyTypeObject) -> *const ShapeTree {
    unsafe {
        if (*tp).tp_flags & Py_TPFLAGS_INLINE_VALUES == 0 || (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE == 0 {
            return std::ptr::null();
        }
        (*(tp as *mut PyHeapTypeObject)).ht_cached_keys as *const ShapeTree
    }
}

/// Offset of the first value slot for instances of `tp`.
#[inline(always)]
fn values_offset(tp: *mut PyTypeObject) -> usize {
    unsafe { ((*tp).tp_basicsize as usize + 7) & !7 }
}

/// Called by `type_new` for a new class with a managed dict: enable inline values if
/// the layout allows (fixed size, generic allocation). `hint` = number of attribute
/// names the compiler saw assigned through `self` (`__static_attributes__`).
pub unsafe fn type_enable(tp: *mut PyTypeObject, hint: usize) {
    unsafe {
        let f = (*tp).tp_flags;
        if f & Py_TPFLAGS_MANAGED_DICT == 0 || f & Py_TPFLAGS_HEAPTYPE == 0 || (*tp).tp_itemsize != 0 || (*tp).tp_dictoffset > 0 {
            return;
        }
        if (*tp).tp_alloc.map(|a| a as usize) != Some(crate::types::typeobject::generic_alloc as usize) {
            return;
        }
        let base = (*tp).tp_base;
        let mut cap = (hint as u32).clamp(1, MAX_INLINE);
        if hint == 0 {
            cap = DEFAULT_CAP;
        }
        if !base.is_null() {
            let bt = tree_of(base);
            if !bt.is_null() {
                cap = cap.max((*bt).cap.load(Relaxed));
            }
        }
        let tree = Box::into_raw(Box::new(ShapeTree {
            root: std::ptr::null(),
            cap: AtomicU32::new(cap),
            nshapes: AtomicU32::new(1),
            inner: Mutex::new(TreeInner { shapes: Vec::new(), keys: Vec::new(), retired: Vec::new() }),
        }));
        let root = Box::into_raw(Box::new(Shape {
            id: NEXT_SHAPE_ID.fetch_add(1, Relaxed),
            len: 0,
            keys: std::ptr::null(),
            parent: std::ptr::null(),
            tree,
            voff: values_offset(tp) as u32,
            trans: AtomicPtr::new(std::ptr::null_mut()),
        }));
        (*tree).root = root;
        (*tree).inner.get_mut().unwrap().shapes.push(root);
        (*(tp as *mut PyHeapTypeObject)).ht_cached_keys = tree as *mut c_void;
        (*tp).tp_flags |= Py_TPFLAGS_INLINE_VALUES;
    }
}

/// Called by `type_dealloc`.
pub unsafe fn type_free(tp: *mut PyTypeObject) {
    unsafe {
        let h = tp as *mut PyHeapTypeObject;
        let t = (*h).ht_cached_keys as *mut ShapeTree;
        if !t.is_null() && (*tp).tp_flags & Py_TPFLAGS_INLINE_VALUES != 0 {
            (*h).ht_cached_keys = std::ptr::null_mut();
            drop(Box::from_raw(t));
        }
    }
}

/// Extra bytes `generic_alloc` adds for the value slots of a new instance of `tp`
/// (0 if the type has no inline values), plus the capacity.
#[inline(always)]
pub unsafe fn alloc_extra(tp: *mut PyTypeObject) -> (usize, u32) {
    let t = tree_of(tp);
    if t.is_null() {
        return (0, 0);
    }
    let cap = unsafe { (*t).cap.load(Relaxed) };
    (cap as usize * 8, cap)
}

/// Set up the value slots at byte offset `off` of freshly allocated (zeroed) `op`.
/// The capacity is rounded up to what the allocation actually holds.
#[inline(always)]
pub unsafe fn init_values(op: *mut PyObject, tp: *mut PyTypeObject, off: usize, cap: u32) {
    unsafe {
        if !crate::alloc::has_managed(op) || off != values_offset(tp) {
            return;
        }
        let usable = crate::alloc::usable_size(op);
        let cap = if usable > off { ((usable - off) / 8).min(MAX_INLINE as usize) as u32 } else { 0 }.max(cap.min(MAX_INLINE));
        (*op).ob_flags.fetch_or((cap as u16) << CAP_SHIFT, Relaxed);
        *crate::alloc::managed_dict_slot(op) = ((*tree_of(tp)).root as usize | 1) as *mut PyObject;
    }
}

// ------------------------------------------------------------------ objects

#[inline(always)]
fn slot_word(op: *mut PyObject) -> Option<&'static AtomicPtr<PyObject>> {
    unsafe {
        if (*(*op).type_ptr()).tp_flags & Py_TPFLAGS_MANAGED_DICT == 0 || !crate::alloc::has_managed(op) {
            return None;
        }
        Some(&*(crate::alloc::managed_dict_slot(op) as *const AtomicPtr<PyObject>))
    }
}

/// Capacity of `op`'s value slots.
#[inline(always)]
fn capacity(op: *mut PyObject) -> u32 {
    unsafe { ((*op).ob_flags.load(Relaxed) >> CAP_SHIFT) as u32 }
}

/// Value slot `i` of `op` (whose current shape is `sh`).
#[inline(always)]
unsafe fn slot(op: *mut PyObject, sh: *const Shape, i: usize) -> &'static AtomicPtr<PyObject> {
    unsafe { &*((op as *const u8).add((*sh).voff as usize + i * 8) as *const AtomicPtr<PyObject>) }
}

/// The current shape of `op` if its attributes are inline, else null.
#[inline(always)]
fn live_shape(op: *mut PyObject) -> *const Shape {
    match slot_word(op) {
        Some(w) => {
            let p = w.load(Acquire) as usize;
            if p & 1 != 0 { (p & !1) as *const Shape } else { std::ptr::null() }
        }
        None => std::ptr::null(),
    }
}

/// Non-null if `op` keeps its attributes inline (kept for callers that test for a
/// values area; the pointer is the shape, not a separate header).
#[inline(always)]
pub fn values_of(op: *mut PyObject) -> *const Shape {
    live_shape(op)
}

/// True if `op` currently keeps its attributes inline (`_testinternalcapi.has_inline_values`).
pub fn has_inline_values(o: &PyObject) -> bool {
    !live_shape(o.as_ptr()).is_null()
}

/// Current shape of `o` (null in dict mode). For JIT guards: `(*shape).id`.
#[inline(always)]
pub fn shape_of(o: &PyObject) -> *const Shape {
    live_shape(o.as_ptr())
}

/// Canonical (interned) key for a lookup: Ok(Some(k)) usable key, Ok(None) the name
/// cannot be in any shape (no interned equal str exists), Err(()) not an exact str.
#[inline(always)]
fn key_for_get(name: &PyObject) -> Result<Option<*mut PyObject>, ()> {
    if pystr::is_interned(name) {
        return Ok(Some(name.as_ptr()));
    }
    if !pystr::is_str_exact(name) {
        return Err(());
    }
    Ok(pystr::lookup_interned(name))
}

pub enum Get {
    /// Object is not in values mode: use the dict path.
    NotValues,
    Found(PyRef),
    Missing,
}

/// Look `name` up in the inline values of `o`.
#[inline]
pub fn get(o: &PyObject, name: &PyObject) -> Get {
    let op = o.as_ptr();
    let w = match slot_word(op) {
        Some(w) => w,
        None => return Get::NotValues,
    };
    if w.load(Relaxed) as usize & 1 == 0 {
        return Get::NotValues;
    }
    let k = match key_for_get(name) {
        Ok(Some(k)) => k,
        Ok(None) => return Get::Missing,
        Err(()) => {
            materialize(o);
            return Get::NotValues;
        }
    };
    unsafe {
        loop {
            let p = w.load(Acquire) as usize;
            if p & 1 == 0 {
                return Get::NotValues; // materialised concurrently
            }
            let sh = (p & !1) as *const Shape;
            match (*sh).index_of(k) {
                None => return Get::Missing,
                Some(i) => {
                    let v = slot(op, sh, i).load(Acquire);
                    if !v.is_null() {
                        return Get::Found(PyRef::from_borrowed(v));
                    }
                    // removed concurrently: re-read the shape
                    if w.load(Acquire) as usize == p {
                        return Get::Missing;
                    }
                }
            }
        }
    }
}

/// Borrowed value at `hint` if the object is in values mode and the shape has `name`
/// there; otherwise a full lookup in the values (`Some(None)`: values mode, missing).
/// `None`: dict mode. Used by the inline caches (racy-read contract as for dicts).
#[inline(always)]
pub fn get_hinted(o: &PyObject, name: *mut PyObject, hint: u64) -> Option<Option<(*mut PyObject, usize)>> {
    let op = o.as_ptr();
    let sh = live_shape(op);
    if sh.is_null() {
        return None;
    }
    unsafe {
        let keys = (*sh).keys();
        let i = if (hint as usize) < keys.len() && keys[hint as usize] == name {
            hint as usize
        } else {
            let k = if pystr::is_interned(&*name) {
                name
            } else {
                match key_for_get(&*name) {
                    Ok(Some(k)) => k,
                    _ => return Some(None),
                }
            };
            match (*sh).index_of(k) {
                Some(i) => i,
                None => return Some(None),
            }
        };
        let p = slot(op, sh, i).load(Acquire);
        if p.is_null() { Some(None) } else { Some(Some((p, i))) }
    }
}

/// Optional writer lock: only needed once other threads exist.
struct WLock(#[allow(dead_code)] Option<dict::ObGuard>);
#[inline(always)]
fn wlock(o: &PyObject) -> WLock {
    if crate::qsbr::multi_threaded() { WLock(Some(dict::ob_lock(o))) } else { WLock(None) }
}

/// Release a value removed from the value slots (deferred while other threads may
/// still be reading it lock-free).
#[inline(always)]
unsafe fn release_old(p: *mut PyObject) {
    if !p.is_null() {
        unsafe { crate::qsbr::decref_deferred(p) }
    }
}

pub enum Set {
    /// Not in values mode (or just materialised): use the dict path.
    NotValues,
    Done,
    /// Deleting a missing attribute.
    Missing,
}

/// Store (`Some`) or delete (`None`) attribute `name` in the inline values of `o`.
#[inline]
pub fn set(o: &PyObject, name: &PyObject, value: Option<&PyObject>) -> Set {
    set_ix(o, name, value).0
}

/// As [`set`]; also returns the slot index used (for cache hints).
pub fn set_ix(o: &PyObject, name: &PyObject, value: Option<&PyObject>) -> (Set, usize) {
    let op = o.as_ptr();
    let w = match slot_word(op) {
        Some(w) if w.load(Relaxed) as usize & 1 != 0 => w,
        _ => return (Set::NotValues, usize::MAX),
    };
    let k: *mut PyObject = if pystr::is_interned(name) {
        name.as_ptr()
    } else if pystr::is_str_exact(name) {
        match value {
            Some(_) => pystr::intern(name.to_ref()).into_raw(), // immortal: no leak of a ref
            None => match pystr::lookup_interned(name) {
                Some(k) => k,
                None => return (Set::Missing, usize::MAX),
            },
        }
    } else {
        materialize(o);
        return (Set::NotValues, usize::MAX);
    };
    unsafe {
        let g = wlock(o);
        let p = w.load(Acquire) as usize;
        if p & 1 == 0 {
            return (Set::NotValues, usize::MAX);
        }
        let shr = &*((p & !1) as *const Shape);
        match (shr.index_of(k), value) {
            (Some(i), Some(val)) => {
                incref(val.as_ptr());
                let old = slot(op, shr, i).swap(val.as_ptr(), AcqRel);
                drop(g);
                release_old(old);
                (Set::Done, i)
            }
            (None, Some(val)) => {
                let len = shr.len;
                if len < capacity(op) {
                    if let Some(next) = shr.add(k) {
                        incref(val.as_ptr());
                        slot(op, shr, len as usize).store(val.as_ptr(), Release);
                        w.store((next as usize | 1) as *mut PyObject, Release);
                        return (Set::Done, len as usize);
                    }
                } else {
                    // learn: future instances get room for more attributes
                    let tree = &*shr.tree;
                    let want = (len + 1 + (len + 1) / 2).min(MAX_INLINE);
                    tree.cap.fetch_max(want, Relaxed);
                }
                drop(g);
                materialize(o);
                (Set::NotValues, usize::MAX)
            }
            (Some(i), None) => {
                if i + 1 == shr.len as usize && !shr.parent.is_null() {
                    // deleting the most recently added attribute: back to the parent
                    let old = slot(op, shr, i).swap(std::ptr::null_mut(), AcqRel);
                    w.store((shr.parent as usize | 1) as *mut PyObject, Release);
                    drop(g);
                    release_old(old);
                    return (Set::Done, i);
                }
                drop(g);
                materialize(o);
                (Set::NotValues, usize::MAX)
            }
            (None, None) => (Set::Missing, usize::MAX),
        }
    }
}

/// Replace the value of an existing attribute at `hint` if the shape has `name` there
/// (JIT/inline-cache store fast path). False: take the generic path.
#[inline(always)]
pub fn replace_hinted(o: &PyObject, name: *mut PyObject, hint: u64, val: &PyObject) -> bool {
    let op = o.as_ptr();
    let w = match slot_word(op) {
        Some(w) if w.load(Relaxed) as usize & 1 != 0 => w,
        _ => return false,
    };
    unsafe {
        let g = wlock(o);
        let p = w.load(Acquire) as usize;
        if p & 1 == 0 {
            return false;
        }
        let sh = (p & !1) as *const Shape;
        let keys = (*sh).keys();
        if (hint as usize) >= keys.len() || keys[hint as usize] != name {
            return false;
        }
        incref(val.as_ptr());
        let old = slot(op, sh, hint as usize).swap(val.as_ptr(), AcqRel);
        drop(g);
        release_old(old);
        true
    }
}

/// Convert the inline values of `o` into a real dict stored in the managed slot (no-op
/// in dict mode). Returns true if the object is now in dict mode with a dict present.
pub fn materialize(o: &PyObject) -> bool {
    let op = o.as_ptr();
    let w = match slot_word(op) {
        Some(w) => w,
        None => return false,
    };
    let p = w.load(Acquire) as usize;
    if p & 1 == 0 {
        return p != 0;
    }
    // build the dict outside the lock (allocation), then swap under it
    let d = dict::new_dict_presized(capacity(op) as usize);
    let _g = dict::ob_lock(o);
    unsafe {
        let p = w.load(Acquire) as usize;
        if p & 1 == 0 {
            return true; // someone else materialised
        }
        let sh = (p & !1) as *const Shape;
        for (i, &k) in (*sh).keys().iter().enumerate() {
            let val = slot(op, sh, i).load(Acquire);
            if !val.is_null() {
                dict::insert_unique(&d, &*k, pystr::str_hash(&*k), &*val);
            }
        }
        w.store(d.into_raw(), Release);
        // the dict took its own references; drop the slots' ones (deferred: lock-free
        // readers may still be looking at the slots)
        for i in 0..(*sh).len as usize {
            let val = slot(op, sh, i).swap(std::ptr::null_mut(), AcqRel);
            if !val.is_null() {
                crate::qsbr::decref_deferred(val);
            }
        }
    }
    true
}

/// Drop the inline values (dealloc / tp_clear). `keep_mode`: reset to the empty root
/// shape (tp_clear, PyObject_ClearManagedDict) instead of leaving values mode.
pub unsafe fn clear_values(op: *mut PyObject, keep_mode: bool) {
    unsafe {
        let w = match slot_word(op) {
            Some(w) => w,
            None => return,
        };
        let p = w.load(Acquire) as usize;
        if p & 1 == 0 {
            return;
        }
        let sh = (p & !1) as *const Shape;
        let root = (*(*sh).tree).root;
        w.store(if keep_mode { (root as usize | 1) as *mut PyObject } else { std::ptr::null_mut() }, Release);
        for i in 0..(*sh).len as usize {
            let val = slot(op, sh, i).swap(std::ptr::null_mut(), AcqRel);
            xdecref(val);
        }
    }
}

/// Visit the inline values (tp_traverse).
pub unsafe fn traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let sh = live_shape(op);
        if sh.is_null() {
            return 0;
        }
        let f = visit.unwrap();
        for i in 0..(*sh).len as usize {
            let val = slot(op, sh, i).load(Acquire);
            if !val.is_null() {
                let r = f(val, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

/// Number of attributes held inline (0 in dict mode).
pub fn inline_len(o: &PyObject) -> usize {
    let s = shape_of(o);
    if s.is_null() { 0 } else { unsafe { (*s).len as usize } }
}

/// Byte offset (from the object) of the byte holding the number of value slots the
/// object has room for (JIT code checks it before appending an attribute).
pub const fn capacity_offset() -> i32 {
    std::mem::offset_of!(PyObject, ob_flags) as i32 + (CAP_SHIFT / 8) as i32
}

/// (jit3) Shape transitions of `tp` that append interned `name` at index `ix`, as
/// (parent, child) pairs: the optimizing JIT compiles attribute-adding stores (e.g. in
/// `__init__`) to "shape == parent and room left -> store, shape = child". Shapes
/// live as long as their type.
pub fn transitions_adding(tp: *mut PyTypeObject, name: *mut PyObject, ix: usize) -> Vec<(*const Shape, *const Shape)> {
    let t = tree_of(tp);
    if t.is_null() {
        return Vec::new();
    }
    let g = unsafe { (*t).inner.lock().unwrap_or_else(|e| e.into_inner()) };
    g.shapes
        .iter()
        .filter_map(|&s| unsafe {
            let s = &*s;
            if s.len as usize == ix + 1 && !s.parent.is_null() && s.keys()[ix] == name { Some((s.parent, s as *const Shape)) } else { None }
        })
        .collect()
}

/// Layout constants for JIT code: byte offsets of (shape.id, shape.len, shape.keys,
/// shape.voff). The managed-dict word holds `shape | 1`; value `i` of an object `o`
/// is at `o + shape.voff + 8 * i`.
pub const fn jit_layout() -> (i32, i32, i32, i32) {
    (
        std::mem::offset_of!(Shape, id) as i32,
        std::mem::offset_of!(Shape, len) as i32,
        std::mem::offset_of!(Shape, keys) as i32,
        std::mem::offset_of!(Shape, voff) as i32,
    )
}
