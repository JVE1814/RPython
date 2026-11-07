//! `set` / `frozenset`: open-addressing hash table in the public `PySetObject` layout
//! (fill/used/mask/table/hash/finger/smalltable/weakreflist), probing with a short
//! linear run followed by perturbation like CPython, so iteration orders match.
//!
//! Thread safety: table accesses happen under the object's `ob_mutex`
//! (`dict::ob_lock`), which is released around Python-level `__eq__` calls; a
//! private `version` counter (after the public fields) detects concurrent changes.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::dict::{self, ob_lock, ObGuard};
use crate::types::typeobject::*;
use crate::types::{bool_, exceptions, int, list, none, str as pystr, tuple};
use std::ffi::{c_int, c_void};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SetEntry {
    pub key: *mut PyObject,
    pub hash: Py_hash_t,
}

pub const MINSIZE: usize = 8;

#[repr(C)]
pub struct PySetObject {
    pub ob_base: PyObject,
    pub fill: Py_ssize_t,
    pub used: Py_ssize_t,
    pub mask: Py_ssize_t,
    pub table: *mut SetEntry,
    pub hash: Py_hash_t,
    pub finger: Py_ssize_t,
    pub smalltable: [SetEntry; MINSIZE],
    pub weakreflist: *mut PyObject,
    /// private: bumped whenever the entry table is replaced (resize/clear)
    pub version: u64,
}

#[unsafe(no_mangle)]
pub static mut PySet_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyFrozenSet_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PySetIter_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn set_type() -> *mut PyTypeObject {
    &raw mut PySet_Type
}
pub fn frozenset_type() -> *mut PyTypeObject {
    &raw mut PyFrozenSet_Type
}
#[inline]
pub fn is_set(o: &PyObject) -> bool {
    o.is_type(set_type()) || is_subtype(o.type_ptr(), set_type())
}
#[inline]
pub fn is_frozenset(o: &PyObject) -> bool {
    o.is_type(frozenset_type()) || is_subtype(o.type_ptr(), frozenset_type())
}
#[inline]
pub fn is_anyset(o: &PyObject) -> bool {
    o.is_type(set_type()) || o.is_type(frozenset_type()) || is_set(o) || is_frozenset(o)
}
#[inline]
pub fn is_anyset_exact(o: &PyObject) -> bool {
    o.is_type(set_type()) || o.is_type(frozenset_type())
}
#[inline]
pub fn so(o: &PyObject) -> &mut PySetObject {
    unsafe { &mut *(o.as_ptr() as *mut PySetObject) }
}

static DUMMY_CELL: u64 = 0;
#[inline(always)]
fn dummy() -> *mut PyObject {
    &DUMMY_CELL as *const u64 as *mut PyObject
}
const LINEAR_PROBES: usize = 9;
const PERTURB_SHIFT: u32 = 5;

#[inline(always)]
fn entry(s: &PySetObject, i: usize) -> &mut SetEntry {
    unsafe { &mut *s.table.add(i) }
}
#[inline(always)]
fn is_active(e: &SetEntry) -> bool {
    !e.key.is_null() && e.key != dummy()
}

// ------------------------------------------------------------------ allocation

pub fn alloc_set(tp: *mut PyTypeObject) -> PyResult {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        init_storage(op);
        Ok(PyRef::from_raw(op))
    }
}
unsafe fn init_storage(op: *mut PyObject) {
    unsafe {
        let s = &mut *(op as *mut PySetObject);
        s.hash = -1;
        s.mask = (MINSIZE - 1) as isize;
        s.table = s.smalltable.as_mut_ptr();
        s.fill = 0;
        s.used = 0;
        s.finger = 0;
    }
}
#[inline]
fn ensure_init(o: &PyObject) {
    let s = so(o);
    if s.table.is_null() {
        unsafe { init_storage(o.as_ptr()) };
    }
}

/// New exact set from an iterable.
pub fn make_set(it: &PyObject) -> PyResult {
    let s = alloc_set(set_type())?;
    set_update(&s, it)?;
    Ok(s)
}
pub fn make_set_empty() -> PyResult {
    alloc_set(set_type())
}
pub fn new_set_from_vec(items: Vec<PyRef>) -> PyResult {
    let s = alloc_set(set_type())?;
    for it in items {
        set_add(&s, &it)?;
    }
    Ok(s)
}
pub fn new_frozenset_from_vec(items: Vec<PyRef>) -> PyResult {
    let s = alloc_set(frozenset_type())?;
    for it in items {
        set_add(&s, &it)?;
    }
    Ok(s)
}
/// New set of the *base* type (set or frozenset) of `tp`, filled from `it`.
fn make_new_set_basetype(tp: *mut PyTypeObject, it: Option<&PyObject>) -> PyResult {
    let base = if is_subtype(tp, set_type()) { set_type() } else { frozenset_type() };
    let s = alloc_set(base)?;
    if let Some(i) = it {
        set_update(&s, i)?;
    }
    Ok(s)
}

// ------------------------------------------------------------------ table operations

enum SFound {
    Active(usize),
    Unused(usize),
}

/// Probe for key; returns with the lock held.
fn find(o: &PyObject, key: &PyObject, hash: isize) -> PyResult<(ObGuard, SFound)> {
    ensure_init(o);
    let kp = key.as_ptr();
    let str_key = pystr::is_str_exact(key);
    'restart: loop {
        let mut g = ob_lock(o);
        let mut mask = so(o).mask as usize;
        let mut perturb = hash as usize;
        let mut i = (hash as usize) & mask;
        loop {
            let probes = if i + LINEAR_PROBES <= mask { LINEAR_PROBES } else { 0 };
            let mut j = 0;
            while j <= probes {
                let idx = i + j;
                let s = so(o);
                let e = *entry(s, idx);
                if e.key.is_null() {
                    return Ok((g, SFound::Unused(idx)));
                }
                if e.hash == hash && e.key != dummy() {
                    if e.key == kp {
                        return Ok((g, SFound::Active(idx)));
                    }
                    let ek = unsafe { &*e.key };
                    if str_key && pystr::is_str_exact(ek) {
                        if pystr::str_eq(ek, key) {
                            return Ok((g, SFound::Active(idx)));
                        }
                    } else {
                        let version = s.version;
                        let ek_owned = ek.to_ref();
                        drop(g);
                        let eq = ab::rich_eq(&ek_owned, key);
                        drop(ek_owned);
                        let eq = eq?;
                        g = ob_lock(o);
                        let s2 = so(o);
                        if s2.version != version || entry(s2, idx).key != e.key {
                            drop(g);
                            continue 'restart;
                        }
                        if eq {
                            return Ok((g, SFound::Active(idx)));
                        }
                        mask = s2.mask as usize;
                    }
                }
                j += 1;
            }
            perturb >>= PERTURB_SHIFT;
            i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb)) & mask;
        }
    }
}

fn insert_clean(table: *mut SetEntry, mask: usize, key: *mut PyObject, hash: isize) {
    let mut perturb = hash as usize;
    let mut i = (hash as usize) & mask;
    unsafe {
        loop {
            let e = &mut *table.add(i);
            if e.key.is_null() {
                e.key = key;
                e.hash = hash;
                return;
            }
            if i + LINEAR_PROBES <= mask {
                for j in 1..=LINEAR_PROBES {
                    let e = &mut *table.add(i + j);
                    if e.key.is_null() {
                        e.key = key;
                        e.hash = hash;
                        return;
                    }
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb)) & mask;
        }
    }
}

/// Resize the table (lock held by caller) so it can hold `minused` entries.
fn table_resize(s: &mut PySetObject, minused: usize) {
    let mut newsize = MINSIZE;
    while newsize <= minused {
        newsize <<= 1;
    }
    let oldtable = s.table;
    let oldmask = s.mask as usize;
    let small = s.smalltable.as_mut_ptr();
    let is_oldtable_malloced = oldtable != small;
    // Snapshot of the live entries (old table may be the small table we reuse).
    let mut live: Vec<SetEntry> = Vec::with_capacity(s.used as usize);
    for i in 0..=oldmask {
        let e = unsafe { *oldtable.add(i) };
        if is_active(&e) {
            live.push(e);
        }
    }
    let newtable = if newsize == MINSIZE {
        unsafe { std::ptr::write_bytes(small, 0, MINSIZE) };
        small
    } else {
        unsafe { crate::alloc::buf_calloc(newsize * std::mem::size_of::<SetEntry>()) as *mut SetEntry }
    };
    for e in &live {
        insert_clean(newtable, newsize - 1, e.key, e.hash);
    }
    s.table = newtable;
    s.mask = (newsize - 1) as isize;
    s.fill = s.used;
    s.version += 1;
    if is_oldtable_malloced {
        unsafe { crate::alloc::buf_free(oldtable as *mut u8) };
    }
}

/// Add with known hash. The caller passes a borrowed key.
pub fn add_entry(o: &PyObject, key: &PyObject, hash: isize) -> PyResult<()> {
    let (g, f) = find(o, key, hash)?;
    if let SFound::Unused(idx) = f {
        let s = so(o);
        unsafe { incref(key.as_ptr()) };
        let e = entry(s, idx);
        e.key = key.as_ptr();
        e.hash = hash;
        s.fill += 1;
        s.used += 1;
        if (s.fill as usize) * 5 >= (s.mask as usize) * 3 {
            let u = s.used as usize;
            table_resize(s, if u > 50000 { u * 2 } else { u * 4 });
        }
    }
    drop(g);
    Ok(())
}

/// Discard with known hash; Ok(true) if found.
pub fn discard_entry(o: &PyObject, key: &PyObject, hash: isize) -> PyResult<bool> {
    if so(o).used == 0 {
        return Ok(false);
    }
    let (g, f) = find(o, key, hash)?;
    match f {
        SFound::Active(idx) => {
            let s = so(o);
            let e = entry(s, idx);
            let old = e.key;
            e.key = dummy();
            e.hash = -1;
            s.used -= 1;
            drop(g);
            unsafe { decref(old) };
            Ok(true)
        }
        SFound::Unused(_) => Ok(false),
    }
}

pub fn contains_entry(o: &PyObject, key: &PyObject, hash: isize) -> PyResult<bool> {
    if so(o).used == 0 {
        return Ok(false);
    }
    if can_borrow(o) {
        ensure_init(o);
        if let Some(r) = find_fast(so(o), key.as_ptr(), hash) {
            return Ok(r.is_some());
        }
    }
    let (_g, f) = find(o, key, hash)?;
    Ok(matches!(f, SFound::Active(_)))
}

// ------------------------------------------------------------------ lock-free fast paths
//
// Set algebra on sets of plain keys (exact str/int, or identical objects) never runs
// Python code, so the tables can be walked directly: no lock, no snapshot vector, no
// reference count traffic. That is safe when nobody else can change the table while
// we look at it: frozensets are immutable once built, and a mutable set cannot be
// changed by another thread while only one thread exists (no Python code runs inside
// these loops, so no thread can start meanwhile). Whenever two keys with equal hashes
// need a real `__eq__` call, the fast path gives up (`None`) and the general,
// locking implementation runs instead.

/// True when the set's table may be read without its lock right now.
#[inline]
fn can_borrow(o: &PyObject) -> bool {
    o.is_type(frozenset_type()) || !crate::qsbr::multi_threaded() || is_frozenset(o)
}

/// Equality of two distinct key objects with equal hashes when it can be decided
/// without calling Python code.
#[inline]
fn plain_eq(a: *mut PyObject, b: *mut PyObject) -> Option<bool> {
    unsafe { ab::plain_eq(&*a, &*b) }
}

#[derive(Clone, Copy)]
enum Fast {
    /// present at this index
    Found(usize),
    /// absent; this empty slot ends the probe sequence
    Absent(usize),
    /// needs a real `__eq__` call
    Unknown,
}

/// Probe for `key` without locking or calling Python code.
#[inline]
fn probe_fast(s: &PySetObject, key: *mut PyObject, hash: isize) -> Fast {
    let mask = s.mask as usize;
    let mut perturb = hash as usize;
    let mut i = (hash as usize) & mask;
    loop {
        let probes = if i + LINEAR_PROBES <= mask { LINEAR_PROBES } else { 0 };
        for j in 0..=probes {
            let idx = i + j;
            let e = *entry(s, idx);
            if e.key.is_null() {
                return Fast::Absent(idx);
            }
            if e.hash == hash && e.key != dummy() {
                if e.key == key {
                    return Fast::Found(idx);
                }
                match plain_eq(e.key, key) {
                    Some(true) => return Fast::Found(idx),
                    Some(false) => {}
                    None => return Fast::Unknown,
                }
            }
        }
        perturb >>= PERTURB_SHIFT;
        i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb)) & mask;
    }
}

/// `probe_fast` as presence: Some(Some(index)), Some(None) = absent, None = unknown.
#[inline]
fn find_fast(s: &PySetObject, key: *mut PyObject, hash: isize) -> Option<Option<usize>> {
    match probe_fast(s, key, hash) {
        Fast::Found(i) => Some(Some(i)),
        Fast::Absent(_) => Some(None),
        Fast::Unknown => None,
    }
}

/// The occupied entries of a set whose table can be borrowed (see `can_borrow`).
#[inline]
fn live_entries(s: &PySetObject) -> impl Iterator<Item = SetEntry> + '_ {
    (0..=(s.mask as usize)).map(move |i| *entry(s, i)).filter(is_active)
}

/// Insert a key known to be absent into a set nobody else can see yet (a result under
/// construction): no comparisons, no lock.
fn insert_unique(r: &PyObject, key: *mut PyObject, hash: isize) {
    let s = so(r);
    unsafe { incref(key) };
    if ((s.fill + 1) as usize) * 5 >= (s.mask as usize) * 3 {
        let u = (s.used + 1) as usize;
        table_resize(s, if u > 50000 { u * 2 } else { u * 4 });
    }
    insert_clean(s.table, s.mask as usize, key, hash);
    s.fill += 1;
    s.used += 1;
}

/// A new exact set/frozenset (`tp`) with the same contents as `src` (borrowable).
fn copy_fast(tp: *mut PyTypeObject, src: &PyObject) -> PyResult {
    let r = alloc_set(tp)?;
    let s = so(src);
    let d = so(&r);
    if s.fill == s.used {
        // no deleted slots: the table can be duplicated position by position
        let n = s.mask as usize + 1;
        if n > MINSIZE {
            let t = unsafe { crate::alloc::buf_calloc(n * std::mem::size_of::<SetEntry>()) } as *mut SetEntry;
            if t.is_null() {
                return Err(memory_error());
            }
            d.table = t;
        }
        unsafe { std::ptr::copy_nonoverlapping(s.table, d.table, n) };
        d.mask = s.mask;
        d.fill = s.used;
        d.used = s.used;
        for e in live_entries(d) {
            unsafe { incref(e.key) };
        }
    } else {
        for e in live_entries(s) {
            insert_unique(&r, e.key, e.hash);
        }
    }
    Ok(r)
}

/// Hash for set operations, with CPython's message rewriting.
fn set_hash(key: &PyObject) -> PyResult<isize> {
    dict::hash_key(key, "set element")
}

/// Hash a key; for unhashable *set* keys, retry with a temporary frozenset.
fn hash_or_frozen(key: &PyObject) -> PyResult<(Option<PyRef>, isize)> {
    if pystr::is_str_exact(key) {
        return Ok((None, pystr::str_hash(key)));
    }
    match ab::hash(key) {
        Ok(h) => Ok((None, h)),
        Err(e) => {
            if is_set(key) && exc_matches(exceptions::TypeError()) {
                clear_exc();
                let f = alloc_set(frozenset_type())?;
                set_update(&f, key)?;
                let h = ab::hash(&f)?;
                return Ok((Some(f), h));
            }
            Err(dict::rewrite_unhashable(key, "set element", e))
        }
    }
}

pub fn set_add(s: &PyObject, k: &PyObject) -> PyResult<()> {
    let h = set_hash(k)?;
    add_entry(s, k, h)
}
pub fn set_discard(s: &PyObject, k: &PyObject) -> PyResult<bool> {
    let (tmp, h) = hash_or_frozen(k)?;
    discard_entry(s, tmp.as_deref().unwrap_or(k), h)
}
pub fn set_contains(s: &PyObject, k: &PyObject) -> PyResult<bool> {
    let (tmp, h) = hash_or_frozen(k)?;
    contains_entry(s, tmp.as_deref().unwrap_or(k), h)
}
pub fn set_len(s: &PyObject) -> usize {
    so(s).used as usize
}

/// Snapshot of (key, hash) pairs in table order.
pub fn items_with_hash(o: &PyObject) -> Vec<(PyRef, isize)> {
    ensure_init(o);
    let _g = ob_lock(o);
    let s = so(o);
    let mut v = Vec::with_capacity(s.used as usize);
    for i in 0..=(s.mask as usize) {
        let e = *entry(s, i);
        if is_active(&e) {
            v.push((unsafe { PyRef::from_borrowed(e.key) }, e.hash));
        }
    }
    v
}
pub fn set_items(o: &PyObject) -> Vec<PyRef> {
    ensure_init(o);
    let _g = if can_borrow(o) { None } else { Some(ob_lock(o)) };
    let s = so(o);
    let mut v = Vec::with_capacity(s.used as usize);
    v.extend(live_entries(s).map(|e| unsafe { PyRef::from_borrowed(e.key) }));
    v
}

pub fn clear(o: &PyObject) {
    ensure_init(o);
    let g = ob_lock(o);
    let s = so(o);
    if s.fill == 0 {
        return;
    }
    let oldtable = s.table;
    let oldmask = s.mask as usize;
    let small = s.smalltable.as_mut_ptr();
    let mut keys = Vec::with_capacity(s.used as usize);
    for i in 0..=oldmask {
        let e = unsafe { *oldtable.add(i) };
        if is_active(&e) {
            keys.push(e.key);
        }
    }
    if oldtable != small {
        unsafe { crate::alloc::buf_free(oldtable as *mut u8) };
    }
    unsafe { std::ptr::write_bytes(small, 0, MINSIZE) };
    s.table = small;
    s.mask = (MINSIZE - 1) as isize;
    s.fill = 0;
    s.used = 0;
    s.version += 1;
    drop(g);
    for k in keys {
        unsafe { decref(k) };
    }
}

/// Merge another set into `o` (set_merge).
fn merge_set(o: &PyObject, other: &PyObject) -> PyResult<()> {
    if o.is(other) || so(other).used == 0 {
        return Ok(());
    }
    ensure_init(other);
    if can_borrow(other) {
        let mut pending: Vec<SetEntry> = Vec::new();
        {
            let g = ob_lock(o);
            let s = so(o);
            let os = so(other);
            let n = os.used;
            if (s.fill + n) * 5 >= s.mask * 3 {
                table_resize(s, ((s.used + n) * 2) as usize);
            }
            if s.fill == 0 {
                let mask = s.mask as usize;
                for e in live_entries(os) {
                    unsafe { incref(e.key) };
                    insert_clean(s.table, mask, e.key, e.hash);
                }
                s.fill = n;
                s.used = n;
                drop(g);
                return Ok(());
            }
            for e in live_entries(os) {
                match probe_fast(s, e.key, e.hash) {
                    Fast::Found(_) => {}
                    Fast::Absent(idx) => {
                        unsafe { incref(e.key) };
                        *entry(s, idx) = e;
                        s.fill += 1;
                        s.used += 1;
                        if (s.fill as usize) * 5 >= (s.mask as usize) * 3 {
                            let u = s.used as usize;
                            table_resize(s, if u > 50000 { u * 2 } else { u * 4 });
                        }
                    }
                    Fast::Unknown => pending.push(e),
                }
            }
            // keep the pending keys alive past the lock: `other` may be a mutable set
            for e in &pending {
                unsafe { incref(e.key) };
            }
            drop(g);
        }
        for e in pending {
            let k = unsafe { PyRef::from_raw(e.key) };
            add_entry(o, &k, e.hash)?;
        }
        return Ok(());
    }
    let items = items_with_hash(other);
    let other_mask = so(other).mask;
    {
        let g = ob_lock(o);
        let s = so(o);
        let n = items.len() as isize;
        if (s.fill + n) * 5 >= s.mask * 3 {
            table_resize(s, ((s.used + n) * 2) as usize);
        }
        if s.fill == 0 {
            if s.mask == other_mask && so(other).fill == so(other).used {
                // same geometry, no dummies: positions are identical
                let ot = so(other);
                let _ = ot;
            }
            let mask = s.mask as usize;
            for (k, h) in items.iter() {
                insert_clean(s.table, mask, k.clone().into_raw(), *h);
            }
            s.fill = n;
            s.used = n;
            drop(g);
            return Ok(());
        }
        drop(g);
    }
    for (k, h) in items {
        add_entry(o, &k, h)?;
    }
    Ok(())
}

/// set_update_internal: add all elements of an iterable.
pub fn set_update(s: &PyObject, it: &PyObject) -> PyResult<()> {
    ensure_init(s);
    if is_anyset(it) {
        return merge_set(s, it);
    }
    if dict::is_dict_exact(it) || crate::types::frozendict::is_frozendict_exact(it) {
        let items = dict::items_hash_vec(it);
        {
            let _g = ob_lock(s);
            let so_ = so(s);
            let n = items.len() as isize;
            if (so_.fill + n) * 5 >= so_.mask * 3 {
                table_resize(so_, ((so_.used + n) * 2) as usize);
            }
        }
        for (k, _, h) in items {
            add_entry(s, &k, h)?;
        }
        return Ok(());
    }
    if list::is_list_exact(it) || tuple::is_tuple_exact(it) {
        let v = if list::is_list_exact(it) { list::list_to_vec(it) } else { tuple::tuple_to_vec(it) };
        for k in v {
            set_add(s, &k)?;
        }
        return Ok(());
    }
    let iter = ab::get_iter(it)?;
    while let Some(k) = ab::iter_next(&iter)? {
        set_add(s, &k)?;
    }
    Ok(())
}

/// Discard every element of `other` from `o`.
pub fn difference_update_with(o: &PyObject, other: &PyObject) -> PyResult<()> {
    if o.is(other) {
        clear(o);
        return Ok(());
    }
    ensure_init(o);
    if is_anyset(other) && so(o).used == 0 {
        // nothing to remove
    } else if is_anyset(other) && !crate::qsbr::multi_threaded() {
        ensure_init(other);
        let mut pending: Vec<(PyRef, isize)> = Vec::new();
        let mut removed: Vec<*mut PyObject> = Vec::new();
        {
            let s = so(o);
            for e in live_entries(so(other)) {
                match probe_fast(s, e.key, e.hash) {
                    Fast::Found(idx) => {
                        let slot = entry(s, idx);
                        removed.push(slot.key);
                        slot.key = dummy();
                        slot.hash = -1;
                        s.used -= 1;
                    }
                    Fast::Absent(_) => {}
                    Fast::Unknown => pending.push((unsafe { PyRef::from_borrowed(e.key) }, e.hash)),
                }
            }
        }
        for k in removed {
            unsafe { decref(k) };
        }
        for (k, h) in pending {
            discard_entry(o, &k, h)?;
        }
    } else if is_anyset(other) {
        for (k, h) in items_with_hash(other) {
            discard_entry(o, &k, h)?;
        }
    } else if dict::is_dict_exact(other) {
        for (k, _, h) in dict::items_hash_vec(other) {
            discard_entry(o, &k, h)?;
        }
    } else {
        let it = ab::get_iter(other)?;
        while let Some(k) = ab::iter_next(&it)? {
            set_discard(o, &k)?;
        }
    }
    // shrink if there are many dummies
    let _g = ob_lock(o);
    let s = so(o);
    if ((s.fill - s.used) as usize) > (s.mask as usize) / 4 {
        let u = s.used as usize;
        table_resize(s, if u > 50000 { u * 2 } else { u * 4 });
    }
    Ok(())
}

pub fn symmetric_difference_update_with(o: &PyObject, other: &PyObject) -> PyResult<()> {
    if o.is(other) {
        clear(o);
        return Ok(());
    }
    if dict::is_dict_exact(other) {
        for (k, _, h) in dict::items_hash_vec(other) {
            if !discard_entry(o, &k, h)? {
                add_entry(o, &k, h)?;
            }
        }
        return Ok(());
    }
    let otherset = if is_anyset(other) { other.to_ref() } else { make_new_set_basetype(o.type_ptr(), Some(other))? };
    for (k, h) in items_with_hash(&otherset) {
        if !discard_entry(o, &k, h)? {
            add_entry(o, &k, h)?;
        }
    }
    Ok(())
}

fn copy_of(o: &PyObject) -> PyResult {
    ensure_init(o);
    if can_borrow(o) {
        let base = if is_subtype(o.type_ptr(), set_type()) { set_type() } else { frozenset_type() };
        return copy_fast(base, o);
    }
    make_new_set_basetype(o.type_ptr(), Some(o))
}

fn intersection_with(o: &PyObject, other: &PyObject) -> PyResult {
    if o.is(other) {
        return copy_of(o);
    }
    let result = make_new_set_basetype(o.type_ptr(), None)?;
    if is_anyset(other) {
        let (small, big) = if set_len(other) > set_len(o) { (o, other) } else { (other, o) };
        ensure_init(small);
        ensure_init(big);
        if can_borrow(small) && can_borrow(big) {
            let mut done = true;
            let bs = so(big);
            for e in live_entries(so(small)) {
                match find_fast(bs, e.key, e.hash) {
                    Some(Some(_)) => insert_unique(&result, e.key, e.hash),
                    Some(None) => {}
                    None => {
                        done = false;
                        break;
                    }
                }
            }
            if done {
                return Ok(result);
            }
            clear(&result);
        }
        for (k, h) in items_with_hash(small) {
            if contains_entry(big, &k, h)? {
                add_entry(&result, &k, h)?;
            }
        }
        return Ok(result);
    }
    let it = ab::get_iter(other)?;
    while let Some(k) = ab::iter_next(&it)? {
        let h = set_hash(&k)?;
        if contains_entry(o, &k, h)? {
            add_entry(&result, &k, h)?;
        }
    }
    Ok(result)
}

fn difference_with(o: &PyObject, other: &PyObject) -> PyResult {
    let other_size = if is_anyset(other) {
        set_len(other)
    } else if dict::is_dict_exact(other) {
        dict::dict_len(other)
    } else {
        let r = copy_of(o)?;
        difference_update_with(&r, other)?;
        return Ok(r);
    };
    if (set_len(o) >> 2) > other_size {
        let r = copy_of(o)?;
        difference_update_with(&r, other)?;
        return Ok(r);
    }
    let result = make_new_set_basetype(o.type_ptr(), None)?;
    let is_d = dict::is_dict_exact(other);
    if !is_d {
        ensure_init(o);
        ensure_init(other);
        if can_borrow(o) && can_borrow(other) {
            let os = so(other);
            let mut done = true;
            for e in live_entries(so(o)) {
                match find_fast(os, e.key, e.hash) {
                    Some(Some(_)) => {}
                    Some(None) => insert_unique(&result, e.key, e.hash),
                    None => {
                        done = false;
                        break;
                    }
                }
            }
            if done {
                return Ok(result);
            }
            clear(&result);
        }
    }
    for (k, h) in items_with_hash(o) {
        let present = if is_d { dict::contains_hash(other, &k, h)? } else { contains_entry(other, &k, h)? };
        if !present {
            add_entry(&result, &k, h)?;
        }
    }
    Ok(result)
}

fn symmetric_difference_with(o: &PyObject, other: &PyObject) -> PyResult {
    let r = make_new_set_basetype(o.type_ptr(), Some(other))?;
    symmetric_difference_update_with(&r, o)?;
    Ok(r)
}

fn issubset_rs(o: &PyObject, other: &PyObject) -> PyResult<bool> {
    if !is_anyset(other) {
        let tmp = make_set(other)?;
        return issubset_rs(o, &tmp);
    }
    if set_len(o) > set_len(other) {
        return Ok(false);
    }
    ensure_init(o);
    ensure_init(other);
    if can_borrow(o) && can_borrow(other) {
        let os = so(other);
        let mut answer = Some(true);
        for e in live_entries(so(o)) {
            match find_fast(os, e.key, e.hash) {
                Some(Some(_)) => {}
                Some(None) => {
                    answer = Some(false);
                    break;
                }
                None => {
                    answer = None;
                    break;
                }
            }
        }
        if let Some(a) = answer {
            return Ok(a);
        }
    }
    for (k, h) in items_with_hash(o) {
        if !contains_entry(other, &k, h)? {
            return Ok(false);
        }
    }
    Ok(true)
}
fn issuperset_rs(o: &PyObject, other: &PyObject) -> PyResult<bool> {
    if is_anyset(other) {
        return issubset_rs(other, o);
    }
    let it = ab::get_iter(other)?;
    while let Some(k) = ab::iter_next(&it)? {
        if !set_contains(o, &k)? {
            return Ok(false);
        }
    }
    Ok(true)
}

// ------------------------------------------------------------------ slots

unsafe extern "C" fn set_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let s = &mut *(o as *mut PySetObject);
        if !s.weakreflist.is_null() {
            crate::types::weakref::clear_weakrefs_at(o, &mut s.weakreflist);
        }
        if !s.table.is_null() {
            for i in 0..=(s.mask as usize) {
                let e = *s.table.add(i);
                if is_active(&e) {
                    decref(e.key);
                }
            }
            if s.table != s.smalltable.as_mut_ptr() {
                crate::alloc::buf_free(s.table as *mut u8);
            }
            s.table = std::ptr::null_mut();
        }
        let tp = (*o).type_ptr();
        generic_free_and_type_decref(o, tp);
    }
}
unsafe extern "C" fn set_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let s = &*(o as *mut PySetObject);
        if s.table.is_null() {
            return 0;
        }
        let f = visit.unwrap();
        for i in 0..=(s.mask as usize) {
            let e = *s.table.add(i);
            if is_active(&e) {
                let r = f(e.key, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}
unsafe extern "C" fn set_clear_slot(o: *mut PyObject) -> c_int {
    unsafe { clear(&*o) };
    0
}

unsafe extern "C" fn set_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let tn = tname(o);
        let r = (|| -> PyResult {
            if set_len(o) == 0 {
                return Ok(pystr::new_str(&format!("{}()", tn)));
            }
            if crate::modules::reprlib_guard::enter(o) {
                return Ok(pystr::new_str(&format!("{}(...)", tn)));
            }
            let res = (|| {
                let mut sb = pystr::StrBuilder::new();
                let exact = o.is_type(set_type());
                if !exact {
                    sb.push_str(&tn);
                    sb.push('(' as u32);
                }
                sb.push('{' as u32);
                for (i, k) in set_items(o).iter().enumerate() {
                    if i > 0 {
                        sb.push_str(", ");
                    }
                    sb.push_obj(&*ab::repr(k)?);
                }
                sb.push('}' as u32);
                if !exact {
                    sb.push(')' as u32);
                }
                Ok(sb.finish())
            })();
            crate::modules::reprlib_guard::leave(o);
            res
        })();
        into_c(r)
    }
}
unsafe extern "C" fn set_len_slot(o: *mut PyObject) -> Py_ssize_t {
    unsafe { so(&*o).used }
}
unsafe extern "C" fn set_contains_slot(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        match set_contains(&*o, &*k) {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}

unsafe extern "C" fn set_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if (tp == set_type() || (*tp).tp_init == (*set_type()).tp_init) && !kw.is_null() && dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "set() takes no keyword arguments");
            }
            let _ = args;
            alloc_set(tp)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn frozenset_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if (tp == frozenset_type() || (*tp).tp_init == (*frozenset_type()).tp_init) && !kw.is_null() && dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "frozenset() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "{} expected at most 1 argument, got {}", type_name(&*tp), a.len());
            }
            if let Some(it) = a.first() {
                if tp == frozenset_type() && it.is_type(frozenset_type()) {
                    return Ok(it.to_ref());
                }
            }
            let s = alloc_set(tp)?;
            if let Some(it) = a.first() {
                set_update(&s, it)?;
            }
            Ok(s)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn set_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            if !kw.is_null() && dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "set() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "set expected at most 1 argument, got {}", a.len());
            }
            let o = &*o;
            if so(o).fill != 0 {
                clear(o);
            }
            if let Some(it) = a.first() {
                set_update(o, it)?;
            }
            Ok(())
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn set_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !is_anyset(b) {
            return none::not_implemented().to_ref().into_raw();
        }
        let r = (|| -> PyResult<bool> {
            let (la, lb) = (set_len(a), set_len(b));
            Ok(match op {
                Py_EQ | Py_NE => {
                    let eq = if la != lb {
                        false
                    } else if is_frozenset(a) && is_frozenset(b) && so(a).hash != -1 && so(b).hash != -1 && so(a).hash != so(b).hash {
                        false
                    } else {
                        issubset_rs(a, b)?
                    };
                    if op == Py_EQ { eq } else { !eq }
                }
                Py_LE => issubset_rs(a, b)?,
                Py_GE => issuperset_rs(a, b)?,
                Py_LT => la < lb && issubset_rs(a, b)?,
                _ => la > lb && issuperset_rs(a, b)?,
            })
        })();
        into_c(r.map(bool_::new_bool))
    }
}

#[inline]
fn shuffle_bits(h: u64) -> u64 {
    ((h ^ 89869747) ^ (h << 16)).wrapping_mul(3644798167)
}

/// The frozenset hash algorithm (also used for frozendict).
pub fn frozenset_hash_of(hashes: impl Iterator<Item = isize>, n: usize) -> isize {
    let mut h: u64 = 0;
    for kh in hashes {
        h ^= shuffle_bits(kh as u64);
    }
    h ^= (n as u64 + 1).wrapping_mul(1927868237);
    h ^= (h >> 11) ^ (h >> 25);
    h = h.wrapping_mul(69069).wrapping_add(907133923);
    let mut r = h as isize;
    if r == -1 {
        r = 590923713;
    }
    r
}

unsafe extern "C" fn frozenset_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let o = &*o;
        let s = so(o);
        if s.hash != -1 {
            return s.hash;
        }
        let items = items_with_hash(o);
        let r = frozenset_hash_of(items.iter().map(|(_, h)| *h), items.len());
        s.hash = r;
        r
    }
}

fn binop_check(a: &PyObject, b: &PyObject) -> bool {
    is_anyset(a) && is_anyset(b)
}
fn nb_sub(a: &PyObject, b: &PyObject) -> PyResult {
    if !binop_check(a, b) {
        return Ok(none::not_implemented().to_ref());
    }
    difference_with(a, b)
}
fn nb_and(a: &PyObject, b: &PyObject) -> PyResult {
    if !binop_check(a, b) {
        return Ok(none::not_implemented().to_ref());
    }
    intersection_with(a, b)
}
fn nb_or(a: &PyObject, b: &PyObject) -> PyResult {
    if !binop_check(a, b) {
        return Ok(none::not_implemented().to_ref());
    }
    let r = copy_of(a)?;
    if !a.is(b) {
        set_update(&r, b)?;
    }
    Ok(r)
}
fn nb_xor(a: &PyObject, b: &PyObject) -> PyResult {
    if !binop_check(a, b) {
        return Ok(none::not_implemented().to_ref());
    }
    symmetric_difference_with(a, b)
}
fn nb_isub(a: &PyObject, b: &PyObject) -> PyResult {
    if !is_anyset(b) {
        return Ok(none::not_implemented().to_ref());
    }
    difference_update_with(a, b)?;
    Ok(a.to_ref())
}
fn nb_iand(a: &PyObject, b: &PyObject) -> PyResult {
    if !is_anyset(b) {
        return Ok(none::not_implemented().to_ref());
    }
    intersection_update_one(a, b)?;
    Ok(a.to_ref())
}
fn nb_ior(a: &PyObject, b: &PyObject) -> PyResult {
    if !is_anyset(b) {
        return Ok(none::not_implemented().to_ref());
    }
    set_update(a, b)?;
    Ok(a.to_ref())
}
fn nb_ixor(a: &PyObject, b: &PyObject) -> PyResult {
    if !is_anyset(b) {
        return Ok(none::not_implemented().to_ref());
    }
    symmetric_difference_update_with(a, b)?;
    Ok(a.to_ref())
}

/// Replace the contents of `o` with those of `src` (swap bodies).
fn swap_bodies(o: &PyObject, src: &PyObject) {
    let g1 = ob_lock(o);
    let g2 = ob_lock(src);
    let a = so(o);
    let b = so(src);
    let a_small = a.table == a.smalltable.as_mut_ptr();
    let b_small = b.table == b.smalltable.as_mut_ptr();
    std::mem::swap(&mut a.fill, &mut b.fill);
    std::mem::swap(&mut a.used, &mut b.used);
    std::mem::swap(&mut a.mask, &mut b.mask);
    std::mem::swap(&mut a.smalltable, &mut b.smalltable);
    let (ta, tb) = (a.table, b.table);
    a.table = if b_small { a.smalltable.as_mut_ptr() } else { tb };
    b.table = if a_small { b.smalltable.as_mut_ptr() } else { ta };
    a.version += 1;
    b.version += 1;
    drop(g2);
    drop(g1);
}

fn intersection_update_one(o: &PyObject, other: &PyObject) -> PyResult<()> {
    let tmp = intersection_with(o, other)?;
    swap_bodies(o, &tmp);
    Ok(())
}

// ------------------------------------------------------------------ methods

fn add_m(o: &PyObject, k: &PyObject) -> PyResult {
    set_add(o, k)?;
    Ok(none::none_ref())
}
fn discard_m(o: &PyObject, k: &PyObject) -> PyResult {
    set_discard(o, k)?;
    Ok(none::none_ref())
}
fn remove_m(o: &PyObject, k: &PyObject) -> PyResult {
    if !set_discard(o, k)? {
        crate::types::dict_methods::raise_key_error(k);
        return Err(PyErr);
    }
    Ok(none::none_ref())
}
fn update_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    for a in args {
        set_update(o, a)?;
    }
    Ok(none::none_ref())
}
fn copy_m(o: &PyObject) -> PyResult {
    if o.is_type(frozenset_type()) {
        return Ok(o.to_ref());
    }
    copy_of(o)
}
fn pop_m(o: &PyObject) -> PyResult {
    ensure_init(o);
    let g = ob_lock(o);
    let s = so(o);
    if s.used == 0 {
        drop(g);
        raise!(KeyError, "pop from an empty set");
    }
    let mask = s.mask as usize;
    let mut i = (s.finger as usize) & mask;
    while !is_active(entry(s, i)) {
        i += 1;
        if i > mask {
            i = 0;
        }
    }
    let e = entry(s, i);
    let key = e.key;
    e.key = dummy();
    e.hash = -1;
    s.used -= 1;
    s.finger = ((i + 1) & mask) as isize;
    drop(g);
    Ok(unsafe { PyRef::from_raw(key) })
}
fn clear_m(o: &PyObject) -> PyResult {
    clear(o);
    Ok(none::none_ref())
}
fn union_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let r = copy_of(o)?;
    for a in args {
        if !a.is(o) {
            set_update(&r, a)?;
        }
    }
    Ok(r)
}
fn intersection_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() {
        return copy_of(o);
    }
    let mut r = o.to_ref();
    for a in args {
        r = intersection_with(&r, a)?;
    }
    if r.is(o) {
        return copy_of(o);
    }
    Ok(r)
}
fn intersection_update_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let tmp = intersection_m(o, args)?;
    swap_bodies(o, &tmp);
    Ok(none::none_ref())
}
fn difference_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() {
        return copy_of(o);
    }
    let r = difference_with(o, args[0])?;
    for a in &args[1..] {
        difference_update_with(&r, a)?;
    }
    Ok(r)
}
fn difference_update_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    for a in args {
        difference_update_with(o, a)?;
    }
    Ok(none::none_ref())
}
fn symmetric_difference_m(o: &PyObject, other: &PyObject) -> PyResult {
    symmetric_difference_with(o, other)
}
fn symmetric_difference_update_m(o: &PyObject, other: &PyObject) -> PyResult {
    symmetric_difference_update_with(o, other)?;
    Ok(none::none_ref())
}
fn issubset_m(o: &PyObject, other: &PyObject) -> PyResult {
    Ok(bool_::new_bool(issubset_rs(o, other)?))
}
fn issuperset_m(o: &PyObject, other: &PyObject) -> PyResult {
    Ok(bool_::new_bool(issuperset_rs(o, other)?))
}
fn isdisjoint_m(o: &PyObject, other: &PyObject) -> PyResult {
    if o.is(other) {
        return Ok(bool_::new_bool(set_len(o) == 0));
    }
    if is_anyset(other) {
        let (small, big) = if set_len(other) > set_len(o) { (o, other) } else { (other, o) };
        for (k, h) in items_with_hash(small) {
            if contains_entry(big, &k, h)? {
                return Ok(bool_::new_bool(false));
            }
        }
        return Ok(bool_::new_bool(true));
    }
    let it = ab::get_iter(other)?;
    while let Some(k) = ab::iter_next(&it)? {
        let h = set_hash(&k)?;
        if contains_entry(o, &k, h)? {
            return Ok(bool_::new_bool(false));
        }
    }
    Ok(bool_::new_bool(true))
}
fn reduce_m(o: &PyObject) -> PyResult {
    let keys = list::new_list(set_items(o));
    let state = crate::modules::copyreg_support::object_getstate(o, false)?;
    Ok(tuple::new_tuple(vec![type_ref(o.type_ptr()), tuple::new_tuple(vec![keys]), state]))
}
fn sizeof_m(o: &PyObject) -> PyResult {
    let s = so(o);
    let mut n = o.ty().tp_basicsize as usize;
    if s.table != s.smalltable.as_mut_ptr() {
        n += (s.mask as usize + 1) * std::mem::size_of::<SetEntry>();
    }
    Ok(int::new_int(n as i64))
}
fn contains_m(o: &PyObject, k: &PyObject) -> PyResult {
    Ok(bool_::new_bool(set_contains(o, k)?))
}
fn class_getitem_m(cls: &PyObject, item: &PyObject) -> PyResult {
    crate::types::containers_reg::generic_alias(cls, item)
}

// ------------------------------------------------------------------ iterator

#[repr(C)]
struct SetIter {
    ob_base: PyObject,
    set: *mut PyObject,
    used: isize,
    pos: isize,
    len: isize,
}
fn new_set_iter(o: &PyObject) -> PyResult {
    ensure_init(o);
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut PySetIter_Type, std::mem::size_of::<SetIter>());
        let it = &mut *(op as *mut SetIter);
        it.set = o.to_ref().into_raw();
        it.used = so(o).used;
        it.len = it.used;
        it.pos = 0;
        crate::gc::track(op);
        Ok(PyRef::from_raw(op))
    }
}
unsafe extern "C" fn set_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(new_set_iter(&*o)) }
}
fn setiter_next_rs(o: &PyObject) -> PyResult<Option<PyRef>> {
    // An iterator shared by several threads: without this, one thread can drop the
    // exhausted set (and clear `set`) while another is still scanning its table.
    let _g = crate::objlock::lock_held(o);
    let it = unsafe { &mut *(o.as_ptr() as *mut SetIter) };
    if it.set.is_null() {
        return Ok(None);
    }
    let s_obj = unsafe { &*it.set };
    if it.used != so(s_obj).used {
        it.used = -1;
        raise!(RuntimeError, "Set changed size during iteration");
    }
    let found = {
        let _g = if can_borrow(s_obj) { None } else { Some(ob_lock(s_obj)) };
        let s = so(s_obj);
        let mask = s.mask as isize;
        let mut i = it.pos;
        while i <= mask && !is_active(entry(s, i as usize)) {
            i += 1;
        }
        it.pos = i + 1;
        if i > mask { None } else { Some(unsafe { PyRef::from_borrowed(entry(s, i as usize).key) }) }
    };
    match found {
        None => {
            let p = std::mem::replace(&mut it.set, std::ptr::null_mut());
            unsafe { decref(p) };
            Ok(None)
        }
        Some(k) => {
            it.len -= 1;
            Ok(Some(k))
        }
    }
}
unsafe extern "C" fn setiter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        match setiter_next_rs(&*o) {
            Ok(Some(v)) => v.into_raw(),
            _ => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn setiter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut SetIter)).set);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn setiter_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let s = (*(o as *mut SetIter)).set;
        if s.is_null() { 0 } else { visit.unwrap()(s, arg) }
    }
}
unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}
fn setiter_len(o: &PyObject) -> PyResult {
    let it = unsafe { &*(o.as_ptr() as *mut SetIter) };
    let n = if !it.set.is_null() && it.used == so(unsafe { &*it.set }).used { it.len } else { 0 };
    Ok(int::new_int(n.max(0) as i64))
}
fn setiter_reduce(o: &PyObject) -> PyResult {
    let iter_fn = crate::builtins::builtin("iter")?;
    let it = unsafe { &*(o.as_ptr() as *mut SetIter) };
    let mut items = Vec::new();
    if !it.set.is_null() {
        let cp = new_set_iter(unsafe { &*it.set })?;
        unsafe {
            let c = &mut *(cp.as_ptr() as *mut SetIter);
            c.used = it.used;
            c.pos = it.pos;
            c.len = it.len;
        }
        while let Some(x) = setiter_next_rs(&cp)? {
            items.push(x);
        }
    }
    Ok(tuple::new_tuple(vec![iter_fn, tuple::new_tuple(vec![list::new_list(items)])]))
}

// ------------------------------------------------------------------ registration

static mut SET_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut SET_NUM: PyNumberMethods = PyNumberMethods::ZERO;
static mut FROZENSET_NUM: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    use crate::methods::binary_shim as bs;
    unsafe {
        let s = &mut *(&raw mut SET_SEQ);
        s.sq_length = Some(set_len_slot);
        s.sq_contains = Some(set_contains_slot);
        let n = &mut *(&raw mut SET_NUM);
        n.nb_subtract = Some(bs(nb_sub));
        n.nb_and = Some(bs(nb_and));
        n.nb_or = Some(bs(nb_or));
        n.nb_xor = Some(bs(nb_xor));
        n.nb_inplace_subtract = Some(bs(nb_isub));
        n.nb_inplace_and = Some(bs(nb_iand));
        n.nb_inplace_or = Some(bs(nb_ior));
        n.nb_inplace_xor = Some(bs(nb_ixor));
        let fnum = &mut *(&raw mut FROZENSET_NUM);
        fnum.nb_subtract = Some(bs(nb_sub));
        fnum.nb_and = Some(bs(nb_and));
        fnum.nb_or = Some(bs(nb_or));
        fnum.nb_xor = Some(bs(nb_xor));
        for (tp, nm) in [(set_type(), c"set"), (frozenset_type(), c"frozenset")] {
            let t = &mut *tp;
            t.tp_name = nm.as_ptr();
            t.tp_basicsize = std::mem::size_of::<PySetObject>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC;
            t.tp_dealloc = Some(set_dealloc);
            t.tp_traverse = Some(set_traverse);
            t.tp_clear = Some(set_clear_slot);
            t.tp_repr = Some(set_repr);
            t.tp_richcompare = Some(set_richcompare);
            t.tp_iter = Some(set_iter);
            t.tp_alloc = Some(generic_alloc);
            t.tp_free = Some(generic_free);
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_weaklistoffset = std::mem::offset_of!(PySetObject, weakreflist) as isize;
            t.tp_as_sequence = s;
        }
        let t = &mut *set_type();
        t.tp_hash = Some(crate::types::slots::hash_not_implemented);
        t.tp_new = Some(set_new);
        t.tp_init = Some(set_init);
        t.tp_as_number = n;
        t.tp_doc = c"set(iterable=(), /)\n--\n\nBuild an unordered collection of unique elements.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("add", O, add_m, "add($self, object, /)\n--\n\nAdd an element to a set.\n\nThis has no effect if the element is already present."),
            ("clear", NOARGS, clear_m, "clear($self, /)\n--\n\nRemove all elements from this set."),
            ("__contains__", O, contains_m, "__contains__($self, object, /)\n--\n\nx.__contains__(y) <==> y in x."; METH_COEXIST),
            ("copy", NOARGS, copy_m, "copy($self, /)\n--\n\nReturn a shallow copy of a set."),
            ("discard", O, discard_m, "discard($self, object, /)\n--\n\nRemove an element from a set if it is a member.\n\nUnlike set.remove(), the discard() method does not raise\nan exception when an element is missing from the set."),
            ("difference", FAST, difference_m, "difference($self, /, *others)\n--\n\nReturn a new set with elements in the set that are not in the others."),
            ("difference_update", FAST, difference_update_m, "difference_update($self, /, *others)\n--\n\nUpdate the set, removing elements found in others."),
            ("intersection", FAST, intersection_m, "intersection($self, /, *others)\n--\n\nReturn a new set with elements common to the set and all others."),
            ("intersection_update", FAST, intersection_update_m, "intersection_update($self, /, *others)\n--\n\nUpdate the set, keeping only elements found in it and all others."),
            ("isdisjoint", O, isdisjoint_m, "isdisjoint($self, other, /)\n--\n\nReturn True if two sets have a null intersection."),
            ("issubset", O, issubset_m, "issubset($self, other, /)\n--\n\nReport whether another set contains this set."),
            ("issuperset", O, issuperset_m, "issuperset($self, other, /)\n--\n\nReport whether this set contains another set."),
            ("pop", NOARGS, pop_m, "pop($self, /)\n--\n\nRemove and return an arbitrary set element.\n\nRaises KeyError if the set is empty."),
            ("__reduce__", NOARGS, reduce_m, "__reduce__($self, /)\n--\n\nReturn state information for pickling."),
            ("remove", O, remove_m, "remove($self, object, /)\n--\n\nRemove an element from a set; it must be a member.\n\nIf the element is not a member, raise a KeyError."),
            ("__sizeof__", NOARGS, sizeof_m, "__sizeof__($self, /)\n--\n\nS.__sizeof__() -> size of S in memory, in bytes."),
            ("symmetric_difference", O, symmetric_difference_m, "symmetric_difference($self, other, /)\n--\n\nReturn a new set with elements in either the set or other but not both."),
            ("symmetric_difference_update", O, symmetric_difference_update_m, "symmetric_difference_update($self, other, /)\n--\n\nUpdate the set, keeping only elements found in either set, but not in both."),
            ("union", FAST, union_m, "union($self, /, *others)\n--\n\nReturn a new set with elements from the set and all others."),
            ("update", FAST, update_m, "update($self, /, *others)\n--\n\nUpdate the set, adding elements from all others."),
            ("__class_getitem__", O, class_getitem_m, "See PEP 585"; METH_CLASS),
        ];
        let t = &mut *frozenset_type();
        t.tp_hash = Some(frozenset_hash);
        t.tp_new = Some(frozenset_new);
        t.tp_as_number = fnum;
        t.tp_doc = c"frozenset(iterable=(), /)\n--\n\nBuild an immutable unordered collection of unique elements.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("__contains__", O, contains_m, "__contains__($self, object, /)\n--\n\nx.__contains__(y) <==> y in x."; METH_COEXIST),
            ("copy", NOARGS, copy_m, "copy($self, /)\n--\n\nReturn a shallow copy of a set."),
            ("difference", FAST, difference_m, "difference($self, /, *others)\n--\n\nReturn a new set with elements in the set that are not in the others."),
            ("intersection", FAST, intersection_m, "intersection($self, /, *others)\n--\n\nReturn a new set with elements common to the set and all others."),
            ("isdisjoint", O, isdisjoint_m, "isdisjoint($self, other, /)\n--\n\nReturn True if two sets have a null intersection."),
            ("issubset", O, issubset_m, "issubset($self, other, /)\n--\n\nReport whether another set contains this set."),
            ("issuperset", O, issuperset_m, "issuperset($self, other, /)\n--\n\nReport whether this set contains another set."),
            ("__reduce__", NOARGS, reduce_m, "__reduce__($self, /)\n--\n\nReturn state information for pickling."),
            ("__sizeof__", NOARGS, sizeof_m, "__sizeof__($self, /)\n--\n\nS.__sizeof__() -> size of S in memory, in bytes."),
            ("symmetric_difference", O, symmetric_difference_m, "symmetric_difference($self, other, /)\n--\n\nReturn a new set with elements in either the set or other but not both."),
            ("union", FAST, union_m, "union($self, /, *others)\n--\n\nReturn a new set with elements from the set and all others."),
            ("__class_getitem__", O, class_getitem_m, "See PEP 585"; METH_CLASS),
        ];
        let t = &mut *(&raw mut PySetIter_Type);
        t.tp_name = c"set_iterator".as_ptr();
        t.tp_basicsize = std::mem::size_of::<SetIter>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(setiter_dealloc);
        t.tp_traverse = Some(setiter_traverse);
        t.tp_iter = Some(self_iter);
        t.tp_iternext = Some(setiter_next);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_methods = crate::mdefs![
            ("__length_hint__", NOARGS, setiter_len, "Private method returning an estimate of len(list(it))."),
            ("__reduce__", NOARGS, setiter_reduce, "Return state information for pickling."),
        ];
    }
}

pub fn ready() -> PyResult<()> {
    type_ready(&raw mut PySetIter_Type)
}
