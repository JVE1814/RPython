//! `dict` core: insertion-ordered open-addressing hash table.
//!
//! `PyDictObject.ma_keys` points at a Rust-owned `DictTable`; `ma_values` is unused (NULL).
//! `ma_used` is kept in sync with the number of live entries (C code reads it inline).
//! The same storage is used by `frozendict` (see frozendict.rs), whose object layout
//! starts with `PyDictObject`.
//!
//! Thread safety: every access to the table goes through the per-object `ob_mutex`
//! (see [`ob_lock`]). The lock is never held while Python code can run (key comparison
//! with a Python `__eq__`, decref of an old value that could trigger `__del__`, ...):
//! lookups that need a Python comparison drop the lock, compare, re-lock and restart
//! when the table changed meanwhile (detected with `version`).
//!
//! Python-level methods and slots live in `dict_methods.rs`.

use crate::abstract_::rich_eq;
use crate::alloc;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use std::sync::atomic::{AtomicU8, Ordering::*};

#[unsafe(no_mangle)]
pub static mut PyDict_Type: PyTypeObject = PyTypeObject::ZERO;

#[inline(always)]
pub fn dict_type() -> *mut PyTypeObject {
    &raw mut PyDict_Type
}
#[inline(always)]
pub fn is_dict(o: &PyObject) -> bool {
    o.is_type(dict_type()) || o.ty().tp_flags & Py_TPFLAGS_DICT_SUBCLASS != 0
}
#[inline(always)]
pub fn is_dict_exact(o: &PyObject) -> bool {
    o.is_type(dict_type())
}
/// dict, dict subclass, frozendict or frozendict subclass (shared storage layout).
#[inline(always)]
pub fn is_anydict(o: &PyObject) -> bool {
    is_dict(o) || crate::types::frozendict::is_frozendict(o)
}

// ------------------------------------------------------------------ per-object lock

/// RAII guard for the per-object mutex (`PyObject.ob_mutex`, PyMutex-compatible:
/// bit 0 = locked). Never hold it while running Python code or decref'ing.
pub struct ObGuard {
    m: *const AtomicU8,
}
impl Drop for ObGuard {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe { (*self.m).store(0, Release) }
    }
}
/// Lock `o`'s `ob_mutex`.
#[inline(always)]
pub fn ob_lock(o: &PyObject) -> ObGuard {
    let m = &o.ob_mutex;
    if m.compare_exchange_weak(0, 1, Acquire, Relaxed).is_err() {
        lock_slow(m);
    }
    ObGuard { m }
}
#[cold]
fn lock_slow(m: &AtomicU8) {
    let mut spins = 0u32;
    loop {
        if m.load(Relaxed) == 0 && m.compare_exchange_weak(0, 1, Acquire, Relaxed).is_ok() {
            return;
        }
        spins += 1;
        if spins < 64 {
            std::hint::spin_loop();
        } else {
            std::thread::yield_now();
        }
    }
}

// ------------------------------------------------------------------ table

#[derive(Clone, Copy)]
pub struct Entry {
    pub hash: isize,
    pub key: *mut PyObject,
    pub value: *mut PyObject,
}

const EMPTY: i32 = -1;
const DUMMY: i32 = -2;
const MIN_SIZE: usize = 8;

pub struct DictTable {
    indices: Vec<i32>,
    /// Entries in insertion order; deleted entries have a NULL key (holes).
    pub entries: Vec<Entry>,
    /// Number of live entries.
    used: usize,
    /// Bumped on every change of the key structure (insert/delete/clear/resize).
    pub version: u64,
}

static DICT_VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A fresh, globally unique dict version stamp. Every mutation of a dict
/// (insert, delete, value replacement, clear, resize) gives it a new stamp,
/// so caches may key on `version(d)` alone.
#[inline]
pub fn next_version() -> u64 {
    DICT_VERSION.fetch_add(1, Relaxed) + 1
}

#[inline]
fn usable_for(size: usize) -> usize {
    (size << 1) / 3
}
fn size_for(min_usable: usize) -> usize {
    let mut size = MIN_SIZE;
    while usable_for(size) < min_usable {
        size <<= 1;
    }
    size
}

impl DictTable {
    pub fn new(cap: usize) -> DictTable {
        let size = size_for(cap);
        DictTable { indices: vec![EMPTY; size], entries: Vec::with_capacity(cap.min(usable_for(size))), used: 0, version: next_version() }
    }
    #[inline]
    fn mask(&self) -> usize {
        self.indices.len() - 1
    }
    #[inline]
    fn find_empty_slot(&self, hash: isize) -> usize {
        let mask = self.mask();
        let mut perturb = hash as usize;
        let mut i = (hash as usize) & mask;
        loop {
            if self.indices[i] < 0 {
                return i;
            }
            perturb >>= 5;
            i = (i.wrapping_mul(5).wrapping_add(perturb).wrapping_add(1)) & mask;
        }
    }
    /// Rebuild with room for at least `min_usable` entries (compacts holes).
    fn resize(&mut self, min_usable: usize) {
        let size = size_for(min_usable.max(self.used));
        let mut live: Vec<Entry> = Vec::with_capacity(usable_for(size));
        live.extend(self.entries.iter().copied().filter(|e| !e.key.is_null()));
        self.indices = vec![EMPTY; size];
        self.entries = live;
        let mask = size - 1;
        for (idx, e) in self.entries.iter().enumerate() {
            let mut perturb = e.hash as usize;
            let mut i = (e.hash as usize) & mask;
            while self.indices[i] != EMPTY {
                perturb >>= 5;
                i = (i.wrapping_mul(5).wrapping_add(perturb).wrapping_add(1)) & mask;
            }
            self.indices[i] = idx as i32;
        }
        self.version = next_version();
    }
    #[inline]
    fn insert_new(&mut self, h: isize, key: *mut PyObject, value: *mut PyObject) {
        if self.entries.len() >= usable_for(self.indices.len()) {
            self.resize(self.used * 3);
        }
        let slot = self.find_empty_slot(h);
        self.indices[slot] = self.entries.len() as i32;
        self.entries.push(Entry { hash: h, key, value });
        self.used += 1;
        self.version = next_version();
    }
    /// Find the index slot holding entry `ix` (which has hash `h`).
    fn slot_of(&self, ix: usize, h: isize) -> usize {
        let mask = self.mask();
        let mut perturb = h as usize;
        let mut i = (h as usize) & mask;
        loop {
            if self.indices[i] == ix as i32 {
                return i;
            }
            perturb >>= 5;
            i = (i.wrapping_mul(5).wrapping_add(perturb).wrapping_add(1)) & mask;
        }
    }
    /// Remove entry `ix`; returns the owned (key, value).
    fn remove(&mut self, ix: usize) -> (*mut PyObject, *mut PyObject) {
        let e = self.entries[ix];
        let s = self.slot_of(ix, e.hash);
        self.indices[s] = DUMMY;
        self.entries[ix].key = std::ptr::null_mut();
        self.entries[ix].value = std::ptr::null_mut();
        self.used -= 1;
        self.version = next_version();
        (e.key, e.value)
    }
    pub fn used(&self) -> usize {
        self.used
    }
    pub fn nentries(&self) -> usize {
        self.entries.len()
    }
}

#[inline(always)]
fn raw_table(d: &PyObject) -> *mut DictTable {
    unsafe {
        let dp = d.as_ptr() as *mut PyDictObject;
        let mut t = (*dp).ma_keys as *mut DictTable;
        if t.is_null() {
            t = Box::into_raw(Box::new(DictTable::new(0)));
            (*dp).ma_keys = t as *mut _;
        }
        t
    }
}

/// Unsynchronized table access (callers must hold the object lock or own the object).
#[inline(always)]
pub fn table(d: &PyObject) -> &mut DictTable {
    unsafe { &mut *raw_table(d) }
}

#[inline(always)]
fn set_used(d: &PyObject, n: usize) {
    unsafe { AtomicIsizePtr::store(&raw mut (*(d.as_ptr() as *mut PyDictObject)).ma_used, n as isize) }
}
struct AtomicIsizePtr;
impl AtomicIsizePtr {
    #[inline(always)]
    unsafe fn store(p: *mut isize, v: isize) {
        unsafe { std::sync::atomic::AtomicIsize::from_ptr(p).store(v, Relaxed) }
    }
}

#[inline(always)]
pub fn dict_len(d: &PyObject) -> usize {
    unsafe { std::sync::atomic::AtomicIsize::from_ptr(&raw mut (*(d.as_ptr() as *mut PyDictObject)).ma_used).load(Relaxed) as usize }
}

/// Allocate a new empty dict.
pub fn new_dict() -> PyRef {
    new_dict_presized(0)
}

pub fn new_dict_presized(n: usize) -> PyRef {
    unsafe {
        let op = alloc::alloc_object(dict_type(), std::mem::size_of::<PyDictObject>());
        let dp = op as *mut PyDictObject;
        (*dp).ma_keys = Box::into_raw(Box::new(DictTable::new(n))) as *mut _;
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}

/// Initialise the dict part of an already allocated dict(-subclass) instance.
pub unsafe fn init_dict_storage(op: *mut PyObject) {
    unsafe {
        let dp = op as *mut PyDictObject;
        if (*dp).ma_keys.is_null() {
            (*dp).ma_keys = Box::into_raw(Box::new(DictTable::new(0))) as *mut _;
        }
    }
}

/// Release the storage of a dict-layout object (dealloc helper).
pub unsafe fn free_storage(op: *mut PyObject) {
    unsafe {
        let dp = op as *mut PyDictObject;
        if !(*dp).ma_keys.is_null() {
            let t = Box::from_raw((*dp).ma_keys as *mut DictTable);
            (*dp).ma_keys = std::ptr::null_mut();
            (*dp).ma_used = 0;
            for e in t.entries.iter() {
                if !e.key.is_null() {
                    decref(e.key);
                    decref(e.value);
                }
            }
        }
    }
}

pub unsafe extern "C" fn dict_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        free_storage(op);
        let tp = (*op).type_ptr();
        crate::types::typeobject::generic_free_and_type_decref(op, tp);
    }
}

pub unsafe extern "C" fn dict_traverse(op: *mut PyObject, visit: visitproc, arg: *mut std::ffi::c_void) -> std::ffi::c_int {
    unsafe {
        let dp = op as *mut PyDictObject;
        if (*dp).ma_keys.is_null() {
            return 0;
        }
        let t = &*((*dp).ma_keys as *const DictTable);
        let f = visit.unwrap();
        for e in t.entries.iter() {
            if !e.key.is_null() {
                let r = f(e.key, arg);
                if r != 0 {
                    return r;
                }
                let r = f(e.value, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

pub unsafe extern "C" fn dict_clear_slot(op: *mut PyObject) -> std::ffi::c_int {
    unsafe { clear(&*op) };
    0
}

// ------------------------------------------------------------------ lookup

pub enum Found {
    At(usize),
    Missing,
}

/// Probe for `key`; returns with the lock held so the caller can act atomically.
/// Python-level `__eq__` calls are made without the lock.
pub fn find(d: &PyObject, key: &PyObject, hash: isize) -> PyResult<(ObGuard, Found)> {
    let str_key = crate::types::str::is_str_exact(key);
    let kp = key.as_ptr();
    'restart: loop {
        let mut g = ob_lock(d);
        let mut perturb = hash as usize;
        let mut i = (hash as usize) & table(d).mask();
        loop {
            let t = table(d);
            let ix = t.indices[i];
            if ix == EMPTY {
                return Ok((g, Found::Missing));
            }
            if ix >= 0 {
                let e = t.entries[ix as usize];
                if e.key == kp {
                    return Ok((g, Found::At(ix as usize)));
                }
                if e.hash == hash {
                    let ek = unsafe { &*e.key };
                    if str_key && crate::types::str::is_str_exact(ek) {
                        if crate::types::str::str_eq(ek, key) {
                            return Ok((g, Found::At(ix as usize)));
                        }
                    } else {
                        let version = t.version;
                        let ek_owned = ek.to_ref();
                        drop(g);
                        let eq = rich_eq(&ek_owned, key);
                        drop(ek_owned);
                        let eq = eq?;
                        g = ob_lock(d);
                        let t2 = table(d);
                        if t2.version != version || t2.entries.get(ix as usize).map(|x| x.key) != Some(e.key) {
                            drop(g);
                            continue 'restart;
                        }
                        if eq {
                            return Ok((g, Found::At(ix as usize)));
                        }
                    }
                }
            }
            perturb >>= 5;
            i = (i.wrapping_mul(5).wrapping_add(perturb).wrapping_add(1)) & table(d).mask();
        }
    }
}

/// Look up `key` with precomputed `hash`. Returns the entry index.
pub fn lookup_index(d: &PyObject, key: &PyObject, hash: isize) -> PyResult<Option<usize>> {
    let (_g, f) = find(d, key, hash)?;
    Ok(match f {
        Found::At(ix) => Some(ix),
        Found::Missing => None,
    })
}

/// Plain hash (no message rewriting).
#[inline]
pub fn hash_of(key: &PyObject) -> PyResult<isize> {
    if crate::types::str::is_str_exact(key) {
        return Ok(crate::types::str::str_hash(key));
    }
    crate::abstract_::hash(key)
}

/// Hash for use as a key of `what` ("dict key", "set element", "frozendict key"):
/// a TypeError is replaced by "cannot use 'T' as a <what> (<original message>)".
#[inline]
pub fn hash_key(key: &PyObject, what: &str) -> PyResult<isize> {
    if crate::types::str::is_str_exact(key) {
        return Ok(crate::types::str::str_hash(key));
    }
    match crate::abstract_::hash(key) {
        Ok(h) => Ok(h),
        Err(e) => Err(rewrite_unhashable(key, what, e)),
    }
}

#[cold]
pub fn rewrite_unhashable(key: &PyObject, what: &str, e: PyErr) -> PyErr {
    if !exc_matches(crate::types::exceptions::TypeError()) {
        return e;
    }
    let orig = take_exc().unwrap();
    let msg = crate::abstract_::str_string(&orig).unwrap_or_else(|_| {
        clear_exc();
        String::new()
    });
    let tn = crate::types::typeobject::tname(key);
    let e = new_err(crate::types::exceptions::TypeError(), &format!("cannot use '{}' as a {} ({})", tn, what, msg));
    if let Some(cur) = PyRefOpt::current() {
        crate::types::exceptions::set_context_and_cause(&cur, Some(&orig));
    }
    e
}
struct PyRefOpt;
impl PyRefOpt {
    fn current() -> Option<PyRef> {
        let p = current_exc_ptr();
        if p.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(p) }) }
    }
}

/// Key-hash for operations on `d` (message depends on dict vs frozendict).
#[inline]
pub fn hash_for(d: &PyObject, key: &PyObject) -> PyResult<isize> {
    if crate::types::str::is_str_exact(key) {
        return Ok(crate::types::str::str_hash(key));
    }
    match crate::abstract_::hash(key) {
        Ok(h) => Ok(h),
        Err(e) => Err(rewrite_unhashable(key, if is_dict(d) { "dict key" } else { "frozendict key" }, e)),
    }
}

/// d[key] → owned value or None (no KeyError raised).
pub fn get_item(d: &PyObject, key: &PyObject) -> PyResult<Option<PyRef>> {
    let h = hash_for(d, key)?;
    get_item_hash(d, key, h)
}

pub fn get_item_hash(d: &PyObject, key: &PyObject, h: isize) -> PyResult<Option<PyRef>> {
    let (g, f) = find(d, key, h)?;
    let r = match f {
        Found::At(ix) => Some(unsafe { PyRef::from_borrowed(table(d).entries[ix].value) }),
        Found::Missing => None,
    };
    drop(g);
    Ok(r)
}

/// Fast lookup by an exact str key; errors from Python `__eq__` are swallowed.
pub fn get_item_str(d: &PyObject, key: &PyObject) -> Option<PyRef> {
    let h = crate::types::str::str_hash(key);
    match get_item_hash(d, key, h) {
        Ok(v) => v,
        Err(_) => {
            clear_exc();
            None
        }
    }
}

pub fn get_item_rs(d: &PyObject, key: &str) -> Option<PyRef> {
    let k = crate::types::str::new_str(key);
    get_item_str(d, &k)
}

pub fn set_item(d: &PyObject, key: &PyObject, value: &PyObject) -> PyResult<()> {
    let h = hash_for(d, key)?;
    set_item_hash(d, key, h, value)
}

pub fn set_item_hash(d: &PyObject, key: &PyObject, h: isize, value: &PyObject) -> PyResult<()> {
    let (g, f) = find(d, key, h)?;
    let t = table(d);
    unsafe { incref(value.as_ptr()) };
    match f {
        Found::At(ix) => {
            let old = t.entries[ix].value;
            t.entries[ix].value = value.as_ptr();
            t.version = next_version();
            drop(g);
            unsafe { decref(old) };
        }
        Found::Missing => {
            unsafe { incref(key.as_ptr()) };
            t.insert_new(h, key.as_ptr(), value.as_ptr());
            set_used(d, t.used);
            drop(g);
        }
    }
    Ok(())
}

/// Insert a key known not to be present (fresh dict build, e.g. copy). Steals nothing.
pub fn insert_unique(d: &PyObject, key: &PyObject, h: isize, value: &PyObject) {
    let g = ob_lock(d);
    let t = table(d);
    unsafe {
        incref(key.as_ptr());
        incref(value.as_ptr());
    }
    t.insert_new(h, key.as_ptr(), value.as_ptr());
    set_used(d, t.used);
    drop(g);
}

pub fn set_item_str(d: &PyObject, key: &str, value: &PyObject) -> PyResult<()> {
    let k = crate::types::str::intern_str(key);
    set_item(d, &k, value)
}

/// d.setdefault(key, default) → the value now stored for key.
pub fn setdefault_hash(d: &PyObject, key: &PyObject, h: isize, default: &PyObject) -> PyResult<PyRef> {
    let (g, f) = find(d, key, h)?;
    let t = table(d);
    let r = match f {
        Found::At(ix) => unsafe { PyRef::from_borrowed(t.entries[ix].value) },
        Found::Missing => {
            unsafe {
                incref(key.as_ptr());
                incref(default.as_ptr());
            }
            t.insert_new(h, key.as_ptr(), default.as_ptr());
            set_used(d, t.used);
            default.to_ref()
        }
    };
    drop(g);
    Ok(r)
}

/// Delete key; Ok(false) if missing.
pub fn del_item(d: &PyObject, key: &PyObject) -> PyResult<bool> {
    let h = hash_for(d, key)?;
    del_item_hash(d, key, h)
}

pub fn del_item_hash(d: &PyObject, key: &PyObject, h: isize) -> PyResult<bool> {
    Ok(pop_item_hash(d, key, h)?.is_some())
}

/// Remove `key` and return its value (None if missing).
pub fn pop_item_hash(d: &PyObject, key: &PyObject, h: isize) -> PyResult<Option<PyRef>> {
    if dict_len(d) == 0 {
        return Ok(None);
    }
    let (g, f) = find(d, key, h)?;
    match f {
        Found::At(ix) => {
            let t = table(d);
            let (k, v) = t.remove(ix);
            set_used(d, t.used);
            drop(g);
            unsafe {
                decref(k);
                Ok(Some(PyRef::from_raw(v)))
            }
        }
        Found::Missing => Ok(None),
    }
}

pub fn pop_item(d: &PyObject, key: &PyObject) -> PyResult<Option<PyRef>> {
    let h = hash_for(d, key)?;
    pop_item_hash(d, key, h)
}

/// Remove entry at index (the caller found it under the lock it still holds).
pub fn remove_at(d: &PyObject, ix: usize, _h: isize) -> (*mut PyObject, *mut PyObject) {
    let t = table(d);
    let r = t.remove(ix);
    set_used(d, t.used);
    r
}

pub fn clear(d: &PyObject) {
    let g = ob_lock(d);
    let t = table(d);
    if t.used == 0 && t.entries.is_empty() {
        return;
    }
    let mut fresh = DictTable::new(0);
    fresh.version = next_version();
    let old = std::mem::replace(t, fresh);
    set_used(d, 0);
    drop(g);
    for e in old.entries.iter() {
        if !e.key.is_null() {
            unsafe {
                decref(e.key);
                decref(e.value);
            }
        }
    }
}

/// Live entry at or after `*pos` (advances `pos` past it).
pub fn next_entry(d: &PyObject, pos: &mut usize) -> Option<(PyRef, PyRef)> {
    let _g = ob_lock(d);
    let t = table(d);
    while *pos < t.entries.len() {
        let e = t.entries[*pos];
        *pos += 1;
        if !e.key.is_null() {
            unsafe {
                return Some((PyRef::from_borrowed(e.key), PyRef::from_borrowed(e.value)));
            }
        }
    }
    None
}

/// Like next_entry, also returning the hash.
pub fn next_entry_hash(d: &PyObject, pos: &mut usize) -> Option<(PyRef, PyRef, isize)> {
    let _g = ob_lock(d);
    let t = table(d);
    while *pos < t.entries.len() {
        let e = t.entries[*pos];
        *pos += 1;
        if !e.key.is_null() {
            unsafe {
                return Some((PyRef::from_borrowed(e.key), PyRef::from_borrowed(e.value), e.hash));
            }
        }
    }
    None
}

/// Number of entry slots (live + holes): the iteration bound.
pub fn nentries(d: &PyObject) -> usize {
    let _g = ob_lock(d);
    table(d).entries.len()
}

/// Snapshot of items (owned).
pub fn items_vec(d: &PyObject) -> Vec<(PyRef, PyRef)> {
    let _g = ob_lock(d);
    let t = table(d);
    let mut v = Vec::with_capacity(t.used);
    for e in t.entries.iter() {
        if !e.key.is_null() {
            unsafe { v.push((PyRef::from_borrowed(e.key), PyRef::from_borrowed(e.value))) };
        }
    }
    v
}

/// Snapshot of (key, value, hash).
pub fn items_hash_vec(d: &PyObject) -> Vec<(PyRef, PyRef, isize)> {
    let _g = ob_lock(d);
    let t = table(d);
    let mut v = Vec::with_capacity(t.used);
    for e in t.entries.iter() {
        if !e.key.is_null() {
            unsafe { v.push((PyRef::from_borrowed(e.key), PyRef::from_borrowed(e.value), e.hash)) };
        }
    }
    v
}

pub fn keys_vec(d: &PyObject) -> Vec<PyRef> {
    let _g = ob_lock(d);
    let t = table(d);
    let mut v = Vec::with_capacity(t.used);
    for e in t.entries.iter() {
        if !e.key.is_null() {
            unsafe { v.push(PyRef::from_borrowed(e.key)) };
        }
    }
    v
}

pub fn values_vec(d: &PyObject) -> Vec<PyRef> {
    let _g = ob_lock(d);
    let t = table(d);
    let mut v = Vec::with_capacity(t.used);
    for e in t.entries.iter() {
        if !e.key.is_null() {
            unsafe { v.push(PyRef::from_borrowed(e.value)) };
        }
    }
    v
}

/// Copy the contents of `src` (any dict layout) into a fresh exact dict.
pub fn copy(d: &PyObject) -> PyResult<PyRef> {
    let items = items_hash_vec(d);
    let n = new_dict_presized(items.len());
    {
        let _g = ob_lock(&n);
        let t = table(&n);
        for (k, v, h) in items {
            t.insert_new(h, k.into_raw(), v.into_raw());
        }
        set_used(&n, t.used);
    }
    Ok(n)
}

/// dict.update from another dict-layout object.
pub fn merge_dict(d: &PyObject, other: &PyObject, overwrite: bool) -> PyResult<()> {
    if d.is(other) {
        return Ok(());
    }
    // Fast path: empty target, exact source → bulk copy (no lookups needed).
    if dict_len(d) == 0 && nentries(d) == 0 {
        let items = items_hash_vec(other);
        let g = ob_lock(d);
        let t = table(d);
        if t.used == 0 {
            if t.indices.len() < size_for(items.len()) {
                *t = DictTable { version: next_version(), ..DictTable::new(items.len()) };
            }
            for (k, v, h) in items {
                t.insert_new(h, k.into_raw(), v.into_raw());
            }
            set_used(d, t.used);
            return Ok(());
        }
        drop(g);
        for (k, v, h) in items {
            if !overwrite && lookup_index(d, &k, h)?.is_some() {
                continue;
            }
            set_item_hash(d, &k, h, &v)?;
        }
        return Ok(());
    }
    // Iterate by position so that mutation of `other` is detected.
    let used0 = dict_len(other);
    let mut pos = 0;
    while let Some((k, v, h)) = next_entry_hash(other, &mut pos) {
        if !overwrite && lookup_index(d, &k, h)?.is_some() {
            continue;
        }
        set_item_hash(d, &k, h, &v)?;
        if dict_len(other) != used0 {
            raise!(RuntimeError, "dict mutated during update");
        }
    }
    Ok(())
}

pub fn contains(d: &PyObject, key: &PyObject) -> PyResult<bool> {
    let h = hash_for(d, key)?;
    Ok(lookup_index(d, key, h)?.is_some())
}

pub fn contains_hash(d: &PyObject, key: &PyObject, h: isize) -> PyResult<bool> {
    Ok(lookup_index(d, key, h)?.is_some())
}

pub fn version(d: &PyObject) -> u64 {
    let _g = ob_lock(d);
    table(d).version
}

/// Pop the last item (LIFO), for popitem. Trims the entry array like CPython.
pub fn pop_last(d: &PyObject) -> Option<(PyRef, PyRef)> {
    let g = ob_lock(d);
    let t = table(d);
    let mut ix = t.entries.len();
    while ix > 0 {
        ix -= 1;
        let e = t.entries[ix];
        if !e.key.is_null() {
            let (k, v) = t.remove(ix);
            t.entries.truncate(ix);
            set_used(d, t.used);
            drop(g);
            return unsafe { Some((PyRef::from_raw(k), PyRef::from_raw(v))) };
        }
    }
    t.entries.clear();
    None
}

/// Entry at position `pos` if live: (key, value). Used by iterators.
pub fn entry_at(d: &PyObject, pos: usize) -> Option<(PyRef, PyRef)> {
    let _g = ob_lock(d);
    let t = table(d);
    match t.entries.get(pos) {
        Some(e) if !e.key.is_null() => unsafe { Some((PyRef::from_borrowed(e.key), PyRef::from_borrowed(e.value))) },
        _ => None,
    }
}

/// Approximate memory used by the table (for __sizeof__).
pub fn table_sizeof(d: &PyObject) -> usize {
    let _g = ob_lock(d);
    let t = table(d);
    t.indices.capacity() * 4 + t.entries.capacity() * std::mem::size_of::<Entry>()
}
