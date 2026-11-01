//! `dict` core: insertion-ordered open-addressing hash table.
//! `PyDictObject.ma_keys` points at a Rust-owned `DictTable`; `ma_values` is unused (NULL).
//! Python-level methods live in `dict_methods.rs`.

use crate::abstract_::rich_eq;
use crate::alloc;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;

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

#[derive(Clone, Copy)]
pub struct Entry {
    pub hash: isize,
    pub key: *mut PyObject,
    pub value: *mut PyObject,
}

const EMPTY: i32 = -1;
const DUMMY: i32 = -2;

pub struct DictTable {
    indices: Vec<i32>,
    pub entries: Vec<Entry>,
    /// Number of live entries.
    used: usize,
    /// Bumped on every structural modification (for iteration/lookup safety).
    pub version: u64,
}

impl DictTable {
    fn new(cap: usize) -> DictTable {
        let mut size = 8;
        while size * 2 < cap * 3 {
            size *= 2;
        }
        DictTable { indices: vec![EMPTY; size], entries: Vec::with_capacity(cap.max(5)), used: 0, version: 0 }
    }
    #[inline]
    fn mask(&self) -> usize {
        self.indices.len() - 1
    }
    fn usable(&self) -> bool {
        // load factor 2/3 on entries (incl. deleted holes)
        (self.entries.len() + 1) * 3 <= self.indices.len() * 2
    }
    fn find_empty_slot(&self, hash: isize) -> usize {
        let mask = self.mask();
        let mut perturb = hash as usize;
        let mut i = (hash as usize) & mask;
        loop {
            if self.indices[i] < 0 {
                return i;
            }
            perturb >>= 5;
            i = (i * 5 + perturb + 1) & mask;
        }
    }
    fn rebuild(&mut self, min_used: usize) {
        let live: Vec<Entry> = self.entries.iter().copied().filter(|e| !e.key.is_null()).collect();
        let mut size = 8;
        let target = (min_used.max(live.len()) + 1) * 3;
        while size * 2 < target {
            size *= 2;
        }
        self.indices = vec![EMPTY; size];
        self.entries = live;
        for (idx, e) in self.entries.iter().enumerate() {
            let slot = {
                let mask = size - 1;
                let mut perturb = e.hash as usize;
                let mut i = (e.hash as usize) & mask;
                while self.indices[i] != EMPTY {
                    perturb >>= 5;
                    i = (i * 5 + perturb + 1) & mask;
                }
                i
            };
            self.indices[slot] = idx as i32;
        }
        self.version = crate::interp::cache::next_dict_version();
    }
}

#[inline(always)]
pub fn table(d: &PyObject) -> &mut DictTable {
    unsafe {
        let dp = d.as_ptr() as *mut PyDictObject;
        if (*dp).ma_keys.is_null() {
            (*dp).ma_keys = Box::into_raw(Box::new(DictTable::new(0))) as *mut _;
        }
        &mut *((*dp).ma_keys as *mut DictTable)
    }
}

#[inline(always)]
fn set_used(d: &PyObject, n: usize) {
    unsafe { (*(d.as_ptr() as *mut PyDictObject)).ma_used = n as isize }
}

pub fn dict_len(d: &PyObject) -> usize {
    unsafe { (*(d.as_ptr() as *const PyDictObject)).ma_used as usize }
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

pub unsafe extern "C" fn dict_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let dp = op as *mut PyDictObject;
        if !(*dp).ma_keys.is_null() {
            let t = Box::from_raw((*dp).ma_keys as *mut DictTable);
            (*dp).ma_keys = std::ptr::null_mut();
            for e in t.entries.iter() {
                if !e.key.is_null() {
                    decref(e.key);
                    decref(e.value);
                }
            }
        }
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

/// Look up `key` with precomputed `hash`. Returns the entry index.
pub fn lookup_index(d: &PyObject, key: &PyObject, hash: isize) -> PyResult<Option<usize>> {
    let str_key = crate::types::str::is_str_exact(key);
    'restart: loop {
        let t: &DictTable = table(d);
        let mask = t.mask();
        let mut perturb = hash as usize;
        let mut i = (hash as usize) & mask;
        loop {
            let ix = t.indices[i];
            if ix == EMPTY {
                return Ok(None);
            }
            if ix >= 0 {
                let e = t.entries[ix as usize];
                if e.key == key.as_ptr() {
                    return Ok(Some(ix as usize));
                }
                if e.hash == hash && !e.key.is_null() {
                    let ek = unsafe { &*e.key };
                    if str_key && crate::types::str::is_str_exact(ek) {
                        if crate::types::str::str_eq(ek, key) {
                            return Ok(Some(ix as usize));
                        }
                    } else {
                        let version = t.version;
                        let ek_owned = ek.to_ref();
                        let eq = rich_eq(&ek_owned, key)?;
                        let t2 = table(d);
                        if t2.version != version || t2.entries.get(ix as usize).map(|x| x.key) != Some(e.key) {
                            continue 'restart;
                        }
                        if eq {
                            return Ok(Some(ix as usize));
                        }
                    }
                }
            }
            perturb >>= 5;
            i = (i * 5 + perturb + 1) & mask;
        }
    }
}

#[inline]
pub fn hash_of(key: &PyObject) -> PyResult<isize> {
    if crate::types::str::is_str_exact(key) {
        return Ok(crate::types::str::str_hash(key));
    }
    crate::abstract_::hash(key)
}

/// d[key] → borrowed-as-owned value or None (no KeyError raised).
pub fn get_item(d: &PyObject, key: &PyObject) -> PyResult<Option<PyRef>> {
    let h = hash_of(key)?;
    get_item_hash(d, key, h)
}

pub fn get_item_hash(d: &PyObject, key: &PyObject, h: isize) -> PyResult<Option<PyRef>> {
    match lookup_index(d, key, h)? {
        Some(ix) => {
            let v = table(d).entries[ix].value;
            Ok(Some(unsafe { PyRef::from_borrowed(v) }))
        }
        None => Ok(None),
    }
}

/// Fast lookup by an interned/exact str key; never runs Python code if all keys are str.
pub fn get_item_str(d: &PyObject, key: &PyObject) -> Option<PyRef> {
    let h = crate::types::str::str_hash(key);
    match lookup_index(d, key, h) {
        Ok(Some(ix)) => Some(unsafe { PyRef::from_borrowed(table(d).entries[ix].value) }),
        Ok(None) => None,
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
    let h = hash_of(key)?;
    set_item_hash(d, key, h, value)
}

pub fn set_item_hash(d: &PyObject, key: &PyObject, h: isize, value: &PyObject) -> PyResult<()> {
    if let Some(ix) = lookup_index(d, key, h)? {
        let t = table(d);
        let old = t.entries[ix].value;
        unsafe { incref(value.as_ptr()) };
        t.entries[ix].value = value.as_ptr();
        t.version = crate::interp::cache::next_dict_version(); // jit: caches key on any mutation
        unsafe { decref(old) };
        return Ok(());
    }
    let t = table(d);
    if !t.usable() {
        t.rebuild(t.used * 2 + 1);
    }
    let slot = t.find_empty_slot(h);
    unsafe {
        incref(key.as_ptr());
        incref(value.as_ptr());
    }
    t.indices[slot] = t.entries.len() as i32;
    t.entries.push(Entry { hash: h, key: key.as_ptr(), value: value.as_ptr() });
    t.used += 1;
    t.version = crate::interp::cache::next_dict_version();
    set_used(d, t.used);
    Ok(())
}

pub fn set_item_str(d: &PyObject, key: &str, value: &PyObject) -> PyResult<()> {
    let k = crate::types::str::intern_str(key);
    set_item(d, &k, value)
}

/// Delete key; Ok(false) if missing.
pub fn del_item(d: &PyObject, key: &PyObject) -> PyResult<bool> {
    let h = hash_of(key)?;
    del_item_hash(d, key, h)
}

pub fn del_item_hash(d: &PyObject, key: &PyObject, h: isize) -> PyResult<bool> {
    let ix = match lookup_index(d, key, h)? {
        Some(ix) => ix,
        None => return Ok(false),
    };
    let (k, v) = remove_at(d, ix, h);
    unsafe {
        decref(k);
        decref(v);
    }
    Ok(true)
}

/// Remove entry at index; returns the (owned) key and value pointers.
pub fn remove_at(d: &PyObject, ix: usize, h: isize) -> (*mut PyObject, *mut PyObject) {
    let t = table(d);
    let mask = t.mask();
    let mut perturb = h as usize;
    let mut i = (h as usize) & mask;
    loop {
        if t.indices[i] == ix as i32 {
            t.indices[i] = DUMMY;
            break;
        }
        perturb >>= 5;
        i = (i * 5 + perturb + 1) & mask;
    }
    let e = t.entries[ix];
    t.entries[ix].key = std::ptr::null_mut();
    t.entries[ix].value = std::ptr::null_mut();
    t.used -= 1;
    t.version = crate::interp::cache::next_dict_version();
    set_used(d, t.used);
    // Holes stay in `entries` until the next rebuild: the DUMMY index slots must
    // keep counting towards the load factor or probing can run out of EMPTY slots.
    (e.key, e.value)
}

pub fn clear(d: &PyObject) {
    let t = table(d);
    let old = std::mem::replace(t, DictTable::new(0));
    t.version = crate::interp::cache::next_dict_version();
    set_used(d, 0);
    for e in old.entries.iter() {
        if !e.key.is_null() {
            unsafe {
                decref(e.key);
                decref(e.value);
            }
        }
    }
}

/// Iterate (snapshot-free) by position: returns next live entry at or after `pos`.
pub fn next_entry(d: &PyObject, pos: &mut usize) -> Option<(PyRef, PyRef)> {
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

/// Snapshot of items (owned).
pub fn items_vec(d: &PyObject) -> Vec<(PyRef, PyRef)> {
    let t = table(d);
    let mut v = Vec::with_capacity(t.used);
    for e in t.entries.iter() {
        if !e.key.is_null() {
            unsafe { v.push((PyRef::from_borrowed(e.key), PyRef::from_borrowed(e.value))) };
        }
    }
    v
}

pub fn keys_vec(d: &PyObject) -> Vec<PyRef> {
    items_vec(d).into_iter().map(|(k, _)| k).collect()
}

pub fn copy(d: &PyObject) -> PyResult<PyRef> {
    let n = new_dict_presized(dict_len(d));
    for (k, v) in items_vec(d) {
        let h = hash_of(&k)?;
        set_item_hash(&n, &k, h, &v)?;
    }
    Ok(n)
}

/// dict.update from another dict.
pub fn merge_dict(d: &PyObject, other: &PyObject, overwrite: bool) -> PyResult<()> {
    for (k, v) in items_vec(other) {
        let h = hash_of(&k)?;
        if !overwrite && lookup_index(d, &k, h)?.is_some() {
            continue;
        }
        set_item_hash(d, &k, h, &v)?;
    }
    Ok(())
}

pub fn contains(d: &PyObject, key: &PyObject) -> PyResult<bool> {
    let h = hash_of(key)?;
    Ok(lookup_index(d, key, h)?.is_some())
}

pub fn version(d: &PyObject) -> u64 {
    table(d).version
}

/// Pop the last item (LIFO), for popitem.
pub fn pop_last(d: &PyObject) -> Option<(PyRef, PyRef)> {
    let t = table(d);
    let mut ix = t.entries.len();
    while ix > 0 {
        ix -= 1;
        let e = t.entries[ix];
        if !e.key.is_null() {
            let (k, v) = remove_at(d, ix, e.hash);
            return unsafe { Some((PyRef::from_raw(k), PyRef::from_raw(v))) };
        }
    }
    None
}
