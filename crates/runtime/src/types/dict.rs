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
/// (stability3) `ob_flags` bit of a dict object: it was materialised from an instance's
/// inline values, i.e. it holds attribute names shared with its class (what CPython
/// calls a split table). Only `_testinternalcapi.has_split_table` reads it.
pub const OBF_FROM_INSTANCE: u16 = 1 << 4; // (not 5-7: sharded.rs)
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
/// Extension critical sections on an object lock the same mutex
/// (capi/pythread.rs), so Python-level operations on that object wait for them; a
/// thread inside its own critical section on the object passes through (`m` null:
/// nothing to release).
pub struct ObGuard {
    m: *const AtomicU8,
}
impl Drop for ObGuard {
    #[inline(always)]
    fn drop(&mut self) {
        if !self.m.is_null() {
            // (scale1) waiters may have parked (bit 1): wake them
            unsafe {
                if (*self.m).compare_exchange(1, 0, Release, Relaxed).is_err() {
                    crate::capi::pythread::mutex_unlock(&*self.m);
                }
            }
        }
    }
}
/// Lock `o`'s `ob_mutex`.
#[inline(always)]
pub fn ob_lock(o: &PyObject) -> ObGuard {
    let m = &o.ob_mutex;
    if m.compare_exchange_weak(0, 1, Acquire, Relaxed).is_err() && !lock_slow(m) {
        return ObGuard { m: std::ptr::null() };
    }
    ObGuard { m }
}
/// Contended: returns false if the calling thread already holds the mutex through a
/// critical section. Long waits (a critical section can run arbitrary code) happen in
/// Blocked mode so that a stop-the-world does not wait for this thread.
#[cold]
fn lock_slow(m: &AtomicU8) -> bool {
    let mut spins = 0u32;
    loop {
        let v = m.load(Relaxed);
        if v & 1 == 0 && m.compare_exchange_weak(v, v | 1, Acquire, Relaxed).is_ok() {
            return true;
        }
        spins += 1;
        if spins == 1 && crate::capi::pythread::cs_holds_mutex(m as *const AtomicU8 as usize) {
            return false;
        }
        if spins == 2 {
            // (scale1) spin briefly, then park (PyMutex protocol) instead of thousands
            // of sched_yield calls: contended writers burned whole cores
            crate::capi::pythread::mutex_lock(m);
            return true;
        }
        if spins < 64 {
            std::hint::spin_loop();
        } else if spins < 2000 {
            std::thread::yield_now();
        } else {
            let ts = crate::threads::current_ptr();
            if ts.is_null() {
                std::thread::yield_now();
                continue;
            }
            let ts = unsafe { &*ts };
            let prev = ts.mode.replace(crate::threads::ThreadMode::Blocked);
            while m.load(Relaxed) & 1 != 0 {
                std::thread::sleep(std::time::Duration::from_micros(50));
            }
            // back in the VM (may wait for a stopped world) before taking the lock
            ts.mode.set(prev);
        }
    }
}

// ------------------------------------------------------------------ table

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Entry {
    pub hash: isize,
    pub key: *mut PyObject,
    pub value: *mut PyObject,
}

const EMPTY: i32 = -1;
const DUMMY: i32 = -2;
const MIN_SIZE: usize = 8;

/// Hash table storage. The header lives in a small-object (TLAB) block pointed to
/// by `ma_keys`; the index array and the entry array share one data block
/// (`[i32; size]` then `[Entry; cap]`), also from the TLAB when small. A fresh
/// dict therefore costs two small-object allocations and no malloc.
/// `entries_p` / `nentries` / `version` are read by JIT-generated code.
#[repr(C)]
pub struct DictTable {
    /// Bumped on every change (insert/delete/value store/clear/resize).
    pub version: u64,
    /// Identity of the index layout: renewed on resize/clear (lookups that ran
    /// Python code restart only if this or the probed entry changed).
    layout: u64,
    /// (jit5) Renewed whenever the set of keys or their entry positions change
    /// (insert of a new key, delete, resize, clear) but not on value replacement:
    /// caches of "key K is at entry ix" (LoadGlobal) stay valid across value stores.
    pub keys_version: u64,
    pub entries_p: *mut Entry,
    /// Entries in insertion order (deleted entries have a NULL key): entries_p[..nentries]
    pub nentries: usize,
    indices_p: *mut i32,
    /// number of index slots (power of two)
    size: usize,
    /// entry capacity
    cap: usize,
    /// Number of live entries.
    used: usize,
    /// Index slots that are not EMPTY (live or DUMMY). popitem trims `nentries` but leaves
    /// DUMMY slots behind, so growth must also be driven by this count, or a table could end
    /// up without any EMPTY slot and lookups of missing keys would probe forever.
    fill: usize,
    data_bytes: usize,
    /// (scale1) Sequence lock for lock-free readers (`lookup_lockfree`): odd while a
    /// writer (holding the object lock) changes the index or entry layout.
    pub wseq: std::sync::atomic::AtomicU64,
    /// (scale1) Set (under the object lock) once a thread other than the owner reads or
    /// writes the table: from then on, replaced data blocks wait for lock-free readers.
    pub(crate) shared_read: std::sync::atomic::AtomicBool,
}

/// A fresh, globally unique dict version stamp (the JIT's allocator). Every
/// mutation of a dict (insert, delete, value replacement, clear, resize) gives it
/// a new stamp, so caches may key on `version(d)` alone.
#[inline]
pub fn next_version() -> u64 {
    crate::interp::cache::next_dict_version()
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

#[inline]
fn raw_alloc(bytes: usize) -> *mut u8 {
    if bytes <= crate::tlab::MAX_SMALL {
        let p = crate::tlab::alloc_small(bytes);
        if p.is_null() {
            std::alloc::handle_alloc_error(std::alloc::Layout::from_size_align(bytes, 8).unwrap());
        }
        {
            return p;
        }
    }
    let p = unsafe { libc::malloc(bytes) as *mut u8 };
    if p.is_null() {
        std::alloc::handle_alloc_error(std::alloc::Layout::from_size_align(bytes, 8).unwrap());
    }
    p
}
#[inline]
unsafe fn raw_free(p: *mut u8, bytes: usize) {
    unsafe {
        if bytes <= crate::tlab::MAX_SMALL {
            crate::tlab::free_small(p);
        } else {
            libc::free(p as *mut libc::c_void);
        }
    }
}

/// Bytes before the index array in a data block: (size, cap) of the block.
const BLOCK_HDR: usize = 16;

impl DictTable {
    /// A table with room for `cap` entries (data block allocated, header by value).
    pub fn new(cap: usize) -> DictTable {
        let size = size_for(cap);
        let ecap = usable_for(size);
        let ibytes = (size * 4 + 7) & !7;
        // (scale1) the block starts with its own (size, cap): a lock-free reader derives
        // everything from the one `indices_p` pointer it loaded (no torn snapshot)
        let bytes = BLOCK_HDR + ibytes + ecap * std::mem::size_of::<Entry>();
        let data = raw_alloc(bytes);
        unsafe {
            *(data as *mut usize) = size;
            *(data as *mut usize).add(1) = ecap;
        }
        let data = unsafe { data.add(BLOCK_HDR) };
        unsafe { std::ptr::write_bytes(data, 0xff, size * 4) };
        DictTable {
            version: next_version(),
            layout: next_version(),
            keys_version: next_version(),
            entries_p: unsafe { data.add(ibytes) as *mut Entry },
            nentries: 0,
            indices_p: data as *mut i32,
            size,
            cap: ecap,
            used: 0,
            fill: 0,
            data_bytes: bytes,
            wseq: std::sync::atomic::AtomicU64::new(0),
            shared_read: std::sync::atomic::AtomicBool::new(false),
        }
    }
    /// Start a change of the index/entry layout (object lock held).
    #[inline(always)]
    fn wbegin(&self) {
        let s = self.wseq.load(Relaxed);
        self.wseq.store(s + 1, Relaxed);
        std::sync::atomic::fence(Release);
    }
    #[inline(always)]
    fn wend(&self) {
        let s = self.wseq.load(Relaxed);
        self.wseq.store(s + 1, Release);
    }
    /// Give up the data block (after a resize swapped it in here): freed now, or after
    /// a grace period while lock-free readers on other threads may still be reading it.
    fn retire_block(&mut self) {
        if self.indices_p.is_null() {
            return;
        }
        let p = unsafe { (std::mem::replace(&mut self.indices_p, std::ptr::null_mut()) as *mut u8).sub(BLOCK_HDR) };
        if !self.shared_read.load(Relaxed) {
            // only the owner ever touched this table: nobody can be reading the block
            unsafe { raw_free(p, self.data_bytes) };
            return;
        }
        unsafe fn free_block(p: *mut u8) {
            unsafe { crate::alloc::buf_free(p) }
        }
        unsafe { crate::qsbr::free_deferred(p, free_block) };
    }
    #[inline(always)]
    fn indices(&self) -> &[i32] {
        unsafe { std::slice::from_raw_parts(self.indices_p, self.size) }
    }
    #[inline(always)]
    fn indices_mut(&mut self) -> &mut [i32] {
        unsafe { std::slice::from_raw_parts_mut(self.indices_p, self.size) }
    }
    /// The entry array (insertion order, holes have a NULL key).
    #[inline(always)]
    pub fn entries(&self) -> &[Entry] {
        unsafe { std::slice::from_raw_parts(self.entries_p, self.nentries) }
    }
    #[inline(always)]
    pub fn entries_mut(&mut self) -> &mut [Entry] {
        unsafe { std::slice::from_raw_parts_mut(self.entries_p, self.nentries) }
    }
    #[inline]
    fn mask(&self) -> usize {
        self.size - 1
    }
    #[inline]
    fn find_empty_slot(&self, hash: isize) -> usize {
        let mask = self.mask();
        let idx = self.indices();
        let mut perturb = hash as usize;
        let mut i = (hash as usize) & mask;
        loop {
            if idx[i] < 0 {
                return i;
            }
            perturb >>= 5;
            i = (i.wrapping_mul(5).wrapping_add(perturb).wrapping_add(1)) & mask;
        }
    }
    /// Rebuild with room for at least `min_usable` entries (compacts holes).
    fn resize(&mut self, min_usable: usize) {
        self.wbegin();
        self.resize_inner(min_usable);
        self.wend();
    }
    fn resize_inner(&mut self, min_usable: usize) {
        let mut fresh = DictTable::new(min_usable.max(self.used));
        {
            let mask = fresh.size - 1;
            let mut n = 0;
            for e in self.entries().iter().filter(|e| !e.key.is_null()) {
                unsafe { *fresh.entries_p.add(n) = *e };
                let mut perturb = e.hash as usize;
                let mut i = (e.hash as usize) & mask;
                let idx = fresh.indices_mut();
                while idx[i] != EMPTY {
                    perturb >>= 5;
                    i = (i.wrapping_mul(5).wrapping_add(perturb).wrapping_add(1)) & mask;
                }
                idx[i] = n as i32;
                n += 1;
            }
            fresh.nentries = n;
            fresh.used = n;
            fresh.fill = n;
        }
        // the old data block goes; the header (and `version`) stay in place
        std::mem::swap(&mut self.entries_p, &mut fresh.entries_p);
        std::mem::swap(&mut self.size, &mut fresh.size);
        std::mem::swap(&mut self.cap, &mut fresh.cap);
        std::mem::swap(&mut self.data_bytes, &mut fresh.data_bytes);
        // (scale1) lock-free readers derive the whole layout from this one pointer:
        // published last, after the block is complete
        let new_ip = fresh.indices_p;
        fresh.indices_p = self.indices_p;
        unsafe { std::sync::atomic::AtomicPtr::from_ptr(&raw mut self.indices_p).store(new_ip, Release) };
        self.nentries = fresh.nentries;
        self.fill = fresh.fill;
        fresh.nentries = 0;
        self.version = next_version();
        self.layout = next_version();
        self.keys_version = next_version();
        // `fresh` holds the old block now: it is retired under this table's rules
        fresh.shared_read.store(self.shared_read.load(Relaxed), Relaxed);
        fresh.retire_block();
        drop(fresh);
    }
    #[inline]
    fn insert_new(&mut self, h: isize, key: *mut PyObject, value: *mut PyObject) {
        if self.nentries >= self.cap || self.fill >= self.cap {
            // (perf4) like CPython: the new index size is at least 3 * used (usable = 2/3 of it)
            self.resize(self.used * 2);
        }
        self.wbegin();
        let slot = self.find_empty_slot(h);
        if self.indices()[slot] == EMPTY {
            self.fill += 1;
        }
        let n = self.nentries;
        // (scale1) entry first, then the index that makes it reachable for lock-free readers
        unsafe { *self.entries_p.add(n) = Entry { hash: h, key, value } };
        unsafe { std::sync::atomic::AtomicI32::from_ptr(self.indices_p.add(slot)).store(n as i32, Release) };
        self.nentries = n + 1;
        self.used += 1;
        self.version = next_version();
        self.keys_version = self.version;
        self.wend();
    }
    /// Find the index slot holding entry `ix` (which has hash `h`).
    fn slot_of(&self, ix: usize, h: isize) -> usize {
        let mask = self.mask();
        let idx = self.indices();
        let mut perturb = h as usize;
        let mut i = (h as usize) & mask;
        loop {
            if idx[i] == ix as i32 {
                return i;
            }
            perturb >>= 5;
            i = (i.wrapping_mul(5).wrapping_add(perturb).wrapping_add(1)) & mask;
        }
    }
    /// Remove entry `ix`; returns the owned (key, value).
    fn remove(&mut self, ix: usize) -> (*mut PyObject, *mut PyObject) {
        let e = self.entries()[ix];
        let s = self.slot_of(ix, e.hash);
        self.wbegin();
        self.indices_mut()[s] = DUMMY;
        let en = &mut self.entries_mut()[ix];
        en.key = std::ptr::null_mut();
        en.value = std::ptr::null_mut();
        self.used -= 1;
        self.version = next_version();
        self.keys_version = self.version;
        self.wend();
        (e.key, e.value)
    }
    pub fn used(&self) -> usize {
        self.used
    }
    pub fn nentries(&self) -> usize {
        self.nentries
    }
}

impl Drop for DictTable {
    fn drop(&mut self) {
        if !self.indices_p.is_null() {
            unsafe { raw_free((self.indices_p as *mut u8).sub(BLOCK_HDR), self.data_bytes) };
            self.indices_p = std::ptr::null_mut();
        }
    }
}

/// Allocate a table header (small-object block) holding `t`.
fn alloc_table(t: DictTable) -> *mut DictTable {
    let p = raw_alloc(std::mem::size_of::<DictTable>()) as *mut DictTable;
    unsafe { std::ptr::write(p, t) };
    p
}
/// Free a header allocated by `alloc_table` (and its data block).
unsafe fn free_table(p: *mut DictTable) {
    unsafe {
        std::ptr::drop_in_place(p);
        raw_free(p as *mut u8, std::mem::size_of::<DictTable>());
    }
}

/// (perf3) The shared table of dicts created empty: never written, so a dict that
/// stays empty (a `**kwargs` nobody passed, an unused `{}`) costs no table
/// allocation. Every write path goes through `table()`, which swaps in a private
/// table first. Its fields stay valid for lock-free and compiled readers (no
/// entries, all index slots EMPTY). All empty dicts sharing its version stamps is
/// sound: they have identical (empty) contents.
fn empty_table() -> *mut DictTable {
    static EMPTY: std::sync::atomic::AtomicPtr<DictTable> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
    let p = EMPTY.load(Acquire);
    if !p.is_null() {
        return p;
    }
    let mut t = DictTable::new(0);
    // capacity 0 marks the shared table (every private table has room for entries)
    t.cap = 0;
    let fresh = alloc_table(t);
    match EMPTY.compare_exchange(std::ptr::null_mut(), fresh, AcqRel, Acquire) {
        Ok(_) => fresh,
        Err(cur) => {
            unsafe { free_table(fresh) };
            cur
        }
    }
}

#[inline(always)]
fn raw_table(d: &PyObject) -> *mut DictTable {
    unsafe {
        let dp = d.as_ptr() as *mut PyDictObject;
        let t = (*dp).ma_keys as *mut DictTable;
        if t.is_null() || (*t).cap == 0 {
            return private_table(dp);
        }
        t
    }
}

#[cold]
unsafe fn private_table(dp: *mut PyDictObject) -> *mut DictTable {
    unsafe {
        // a reader without the lock may get here at the same time as a writer:
        // only one fresh table may be installed
        let slot = std::sync::atomic::AtomicPtr::from_ptr(&raw mut (*dp).ma_keys);
        let seen = slot.load(Acquire);
        if !seen.is_null() && (*(seen as *mut DictTable)).cap != 0 {
            return seen as *mut DictTable;
        }
        let t = alloc_table(DictTable::new(0));
        match slot.compare_exchange(seen, t as *mut _, AcqRel, Acquire) {
            Ok(_) => t,
            Err(cur) => {
                free_table(t);
                cur as *mut DictTable
            }
        }
    }
}

/// Does `d` still use the shared empty table?
#[inline(always)]
unsafe fn is_shared_empty(d: &PyObject) -> bool {
    unsafe {
        let t = (*(d.as_ptr() as *mut PyDictObject)).ma_keys as *mut DictTable;
        !t.is_null() && (*t).cap == 0
    }
}

/// Unsynchronized table access (callers must hold the object lock or own the object).
#[inline(always)]
pub fn table(d: &PyObject) -> &mut DictTable {
    unsafe {
        let t = raw_table(d);
        // (scale1) a thread other than the owner works on the table (with the lock held,
        // as every caller that is not the owner does): see `shared_read`
        if !(*t).shared_read.load(Relaxed) && d.ob_tid.load(Relaxed) != thread_id() {
            (*t).shared_read.store(true, Relaxed);
        }
        &mut *t
    }
}

/// Before a thread other than the owner reads `d` without the lock for the first time:
/// mark the table (under the lock, so that a writer that frees blocks immediately
/// cannot be running).
#[inline(always)]
fn mark_shared_read(d: &PyObject, tp: *mut DictTable) {
    unsafe {
        if !(*tp).shared_read.load(Relaxed) && (*tp).cap != 0 && d.ob_tid.load(Relaxed) != thread_id() {
            mark_shared_read_slow(d);
        }
    }
}
/// For code outside this module that reads a dict's entries without the lock
/// (the specialiser's hinted lookups).
pub fn prepare_unlocked_read(d: &PyObject) {
    unsafe {
        let tp = (*(d.as_ptr() as *mut PyDictObject)).ma_keys as *mut DictTable;
        if !tp.is_null() {
            mark_shared_read(d, tp);
        }
    }
}
#[cold]
fn mark_shared_read_slow(d: &PyObject) {
    let g = ob_lock(d);
    let _ = table(d);
    drop(g);
}

/// Table for reading only: may be the shared empty table (never written through).
#[inline(always)]
fn table_ro(d: &PyObject) -> &DictTable {
    unsafe {
        let t = (*(d.as_ptr() as *mut PyDictObject)).ma_keys as *mut DictTable;
        if t.is_null() { &*raw_table(d) } else { &*t }
    }
}

#[inline(always)]
fn set_used(d: &PyObject, n: usize, key: *mut PyObject) {
    unsafe { AtomicIsizePtr::store(&raw mut (*(d.as_ptr() as *mut PyDictObject)).ma_used, n as isize) }
    crate::interp::cache::on_dict_write(d, key); // jit: type-dict writes invalidate caches
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
        (*dp).ma_keys = if n == 0 { empty_table() } else { alloc_table(DictTable::new(n)) } as *mut _;
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}

/// Initialise the dict part of an already allocated dict(-subclass) instance.
pub unsafe fn init_dict_storage(op: *mut PyObject) {
    unsafe {
        let dp = op as *mut PyDictObject;
        if (*dp).ma_keys.is_null() {
            (*dp).ma_keys = alloc_table(DictTable::new(0)) as *mut _;
        }
    }
}

/// Release the storage of a dict-layout object (dealloc helper).
pub unsafe fn free_storage(op: *mut PyObject) {
    unsafe {
        let dp = op as *mut PyDictObject;
        if !(*dp).ma_keys.is_null() && (*((*dp).ma_keys as *mut DictTable)).cap == 0 {
            (*dp).ma_keys = std::ptr::null_mut();
            (*dp).ma_used = 0;
        } else if !(*dp).ma_keys.is_null() {
            let t = (*dp).ma_keys as *mut DictTable;
            (*dp).ma_keys = std::ptr::null_mut();
            (*dp).ma_used = 0;
            for e in (*t).entries().iter() {
                if !e.key.is_null() {
                    decref(e.key);
                    decref(e.value);
                }
            }
            free_table(t);
        }
    }
}

pub unsafe extern "C" fn dict_dealloc(op: *mut PyObject) {
    unsafe {
        let wm = crate::capi::dict::watched_mask(op);
        if wm != 0 {
            // the callbacks see a live dict: resurrect it for their duration
            // (same protocol as gc::call_finalizer_from_dealloc)
            let o = &*op;
            o.ob_ref_local.store(1, std::sync::atomic::Ordering::Relaxed);
            o.ob_tid.store(crate::object::thread_id(), std::sync::atomic::Ordering::Relaxed);
            crate::capi::dict::notify(op, wm, crate::capi::dict::EV_DEALLOCATED, std::ptr::null_mut(), std::ptr::null_mut());
            crate::capi::dict::forget_watched(op);
            let local = o.ob_ref_local.load(std::sync::atomic::Ordering::Relaxed) - 1;
            o.ob_ref_local.store(local, std::sync::atomic::Ordering::Relaxed);
            if local != 0 || (o.ob_ref_shared.load(std::sync::atomic::Ordering::Relaxed) >> 2) != 0 {
                return; // a callback kept it alive
            }
        }
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
        for e in t.entries().iter() {
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
        let mut i = (hash as usize) & table_ro(d).mask();
        loop {
            let t = table_ro(d);
            let ix = t.indices()[i];
            if ix == EMPTY {
                return Ok((g, Found::Missing));
            }
            if ix >= 0 {
                let e = t.entries()[ix as usize];
                if e.key == kp {
                    return Ok((g, Found::At(ix as usize)));
                }
                if e.hash == hash {
                    let ek = unsafe { &*e.key };
                    if str_key && crate::types::str::is_str_exact(ek) {
                        if crate::types::str::str_eq(ek, key) {
                            return Ok((g, Found::At(ix as usize)));
                        }
                    } else if let Some(eq) = crate::abstract_::plain_eq(ek, key) {
                        // decided without Python code: the lock stays held (perf3)
                        if eq {
                            return Ok((g, Found::At(ix as usize)));
                        }
                    } else {
                        let layout = t.layout;
                        let ek_owned = ek.to_ref();
                        drop(g);
                        let eq = rich_eq(&ek_owned, key);
                        drop(ek_owned);
                        let eq = eq?;
                        g = ob_lock(d);
                        let t2 = table_ro(d);
                        if t2.layout != layout || t2.entries().get(ix as usize).map(|x| x.key) != Some(e.key) {
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
            i = (i.wrapping_mul(5).wrapping_add(perturb).wrapping_add(1)) & table_ro(d).mask();
        }
    }
}

/// Look up `key` with precomputed `hash`. Returns the entry index.
// ------------------------------------------------------------------ lock-free reads (scale1)
//
// Readers that only need identity or exact-str key comparison (no Python code) read
// the table without the object lock, validated by the table's sequence lock (`wseq`,
// odd while a writer changes the index/entry layout). What they may still be looking
// at after a concurrent change stays valid until they reach a quiescent point: old
// data blocks (`retire_block`), removed keys and replaced values (`qsbr::decref_deferred`)
// are released through QSBR while other threads exist, and table headers live as long
// as their dict. A value read under a valid snapshot was the value at that moment;
// it is still alive, so taking a reference to it afterwards is safe. Contended dict
// locks were the scaling bottleneck of shared read-mostly dicts (module globals,
// class dicts, lookup tables): every reader wrote the lock word.

enum LfRead {
    /// entry index, value (null while being removed: undecided)
    Hit(usize, *mut PyObject),
    Miss,
    /// needs a comparison that can run Python code, or kept changing
    Undecided,
}

/// The current data block of table `tp`, read without the lock: (indices, entries,
/// index slots, entry capacity), all derived from the one `indices_p` pointer, so they
/// always describe the same block (the current one or one retired after this thread's
/// last quiescent point).
#[inline(always)]
unsafe fn block_layout(tp: *mut DictTable) -> Option<(*mut i32, *mut Entry, usize, usize)> {
    unsafe {
        let ip = std::sync::atomic::AtomicPtr::from_ptr(&raw mut (*tp).indices_p).load(Acquire);
        if ip.is_null() {
            return None;
        }
        let hdr = (ip as *mut u8).sub(BLOCK_HDR) as *const usize;
        let size = *hdr;
        let cap = *hdr.add(1);
        let ep = (ip as *mut u8).add((size * 4 + 7) & !7) as *mut Entry;
        Some((ip, ep, size, cap))
    }
}

/// (scale2) Key equality decided without running Python code (so a lock-free reader
/// may evaluate it): same-type str/int/float/tuple keys (`plain_eq`), and a str
/// against an int or float, which never compare equal. Ints and other keys besides
/// str used to send every lookup to the locked path (a shared dict with int keys was
/// a lock ping-pong).
#[inline]
fn pure_key_eq(a: &PyObject, b: &PyObject) -> Option<bool> {
    if let Some(r) = crate::abstract_::plain_eq(a, b) {
        return Some(r);
    }
    let num = |o: &PyObject| crate::types::int::is_int_exact(o) || o.is_type(crate::types::float::float_type());
    let s = |o: &PyObject| crate::types::str::is_str_exact(o);
    if (s(a) && num(b)) || (num(a) && s(b)) {
        return Some(false);
    }
    None
}

#[inline]
fn lookup_lockfree_raw(d: &PyObject, key: &PyObject, hash: isize) -> LfRead {
    use std::sync::atomic::{AtomicI32, AtomicIsize, AtomicPtr};
    let kp = key.as_ptr();
    let str_key = crate::types::str::is_str_exact(key);
    unsafe {
        let tp = AtomicPtr::from_ptr(&raw mut (*(d.as_ptr() as *mut PyDictObject)).ma_keys).load(Acquire) as *mut DictTable;
        if tp.is_null() {
            return LfRead::Undecided;
        }
        mark_shared_read(d, tp);
        for _ in 0..8 {
            let s1 = (*tp).wseq.load(Acquire);
            if s1 & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let Some((ip, ep, size, cap)) = block_layout(tp) else {
                return LfRead::Undecided;
            };
            let mut res = LfRead::Undecided;
            {
                let mask = size - 1;
                let mut perturb = hash as usize;
                let mut i = (hash as usize) & mask;
                for _ in 0..=size {
                    let ix = AtomicI32::from_ptr(ip.add(i)).load(Relaxed);
                    if ix == EMPTY {
                        res = LfRead::Miss;
                        break;
                    }
                    if ix >= 0 {
                        if ix as usize >= cap {
                            break; // torn snapshot: decided by the sequence check
                        }
                        let e = ep.add(ix as usize);
                        let ek = AtomicPtr::from_ptr(&raw mut (*e).key).load(Relaxed);
                        if ek == kp {
                            res = LfRead::Hit(ix as usize, AtomicPtr::from_ptr(&raw mut (*e).value).load(Acquire));
                            break;
                        }
                        if !ek.is_null() && AtomicIsize::from_ptr(&raw mut (*e).hash).load(Relaxed) == hash {
                            let eq = if str_key && crate::types::str::is_str_exact(&*ek) { Some(crate::types::str::str_eq(&*ek, key)) } else { pure_key_eq(&*ek, key) };
                            match eq {
                                Some(true) => {
                                    res = LfRead::Hit(ix as usize, AtomicPtr::from_ptr(&raw mut (*e).value).load(Acquire));
                                    break;
                                }
                                Some(false) => {}
                                None => break, // a comparison that may run Python code: locked path
                            }
                        }
                    }
                    perturb >>= 5;
                    i = (i.wrapping_mul(5).wrapping_add(perturb).wrapping_add(1)) & mask;
                }
            }
            std::sync::atomic::fence(Acquire);
            if (*tp).wseq.load(Relaxed) != s1 {
                continue;
            }
            if AtomicPtr::from_ptr(&raw mut (*(d.as_ptr() as *mut PyDictObject)).ma_keys).load(Acquire) as *mut DictTable != tp {
                return LfRead::Undecided; // a private table replaced the shared empty one
            }
            return match res {
                LfRead::Hit(_, v) if v.is_null() => LfRead::Undecided,
                r => r,
            };
        }
        LfRead::Undecided
    }
}

/// d[key] without the object lock: Some(result) when it could be decided that way.
#[inline]
fn get_lockfree(d: &PyObject, key: &PyObject, hash: isize) -> Option<Option<PyRef>> {
    match lookup_lockfree_raw(d, key, hash) {
        LfRead::Hit(_, v) => Some(Some(unsafe { PyRef::from_borrowed(v) })),
        LfRead::Miss => Some(None),
        LfRead::Undecided => None,
    }
}

/// (tail4) Like `lookup_index`, but never runs Python code: None when deciding would
/// need a key comparison through `__eq__` (a non-str key with the same hash).
pub fn lookup_index_no_eq(d: &PyObject, key: &PyObject, hash: isize) -> Option<Option<usize>> {
    match lookup_lockfree_raw(d, key, hash) {
        LfRead::Hit(ix, _) => Some(Some(ix)),
        LfRead::Miss => Some(None),
        LfRead::Undecided => None,
    }
}

pub fn lookup_index(d: &PyObject, key: &PyObject, hash: isize) -> PyResult<Option<usize>> {
    match lookup_lockfree_raw(d, key, hash) {
        LfRead::Hit(ix, _) => return Ok(Some(ix)),
        LfRead::Miss => return Ok(None),
        LfRead::Undecided => {}
    }
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
    if let Some(r) = get_lockfree(d, key, h) {
        return Ok(r);
    }
    let (g, f) = find(d, key, h)?;
    let r = match f {
        Found::At(ix) => Some(unsafe { PyRef::from_borrowed(table(d).entries()[ix].value) }),
        Found::Missing => None,
    };
    drop(g);
    Ok(r)
}

/// (jit5, LoadGlobal caches) Entry index of the exact-str `key` in exact dict `d`
/// (None: absent) together with the dict's `keys_version`, read under the dict lock.
/// None if the lookup failed (a key's `__eq__` raised; the error is cleared).
pub fn str_key_index(d: &PyObject, key: &PyObject) -> Option<(Option<usize>, u64)> {
    let h = crate::types::str::str_hash(key);
    match find(d, key, h) {
        Ok((g, f)) => {
            let kv = table(d).keys_version;
            drop(g);
            Some((if let Found::At(ix) = f { Some(ix) } else { None }, kv))
        }
        Err(_) => {
            clear_exc();
            None
        }
    }
}

/// (jit5) The value (new reference) at entry `ix` of exact dict `d` if its key layout
/// is still `keys_version`.
pub fn value_at_if(d: &PyObject, keys_version: u64, ix: usize) -> Option<PyRef> {
    // (scale1) without the lock, validated by the sequence lock (see lookup_lockfree_raw)
    unsafe {
        use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize};
        let tp = AtomicPtr::from_ptr(&raw mut (*(d.as_ptr() as *mut PyDictObject)).ma_keys).load(Acquire) as *mut DictTable;
        if !tp.is_null() {
            mark_shared_read(d, tp);
            let s1 = (*tp).wseq.load(Acquire);
            let lay = if s1 & 1 == 0 { block_layout(tp) } else { None };
            if let Some((_, ep, _, cap)) = lay {
                let kv = AtomicU64::from_ptr(&raw mut (*tp).keys_version).load(Relaxed);
                let n = AtomicUsize::from_ptr(&raw mut (*tp).nentries).load(Relaxed);
                let v = if kv == keys_version && ix < n && ix < cap { AtomicPtr::from_ptr(&raw mut (*ep.add(ix)).value).load(Acquire) } else { std::ptr::null_mut() };
                std::sync::atomic::fence(Acquire);
                if (*tp).wseq.load(Relaxed) == s1 {
                    if kv != keys_version || ix >= n {
                        return None;
                    }
                    if !v.is_null() {
                        return Some(PyRef::from_borrowed(v));
                    }
                }
            }
        }
    }
    let g = ob_lock(d);
    let t = table(d);
    let r = if t.keys_version == keys_version && ix < t.nentries {
        let v = t.entries()[ix].value;
        if v.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(v) }) }
    } else {
        None
    };
    drop(g);
    r
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
    let wm = crate::capi::dict::watched_mask(d.as_ptr());
    if wm != 0 {
        let ev = if contains_hash(d, key, h)? { crate::capi::dict::EV_MODIFIED } else { crate::capi::dict::EV_ADDED };
        crate::capi::dict::notify(d.as_ptr(), wm, ev, key.as_ptr(), value.as_ptr());
    }
    let (g, f) = find(d, key, h)?;
    let t = table(d);
    unsafe { incref(value.as_ptr()) };
    match f {
        Found::At(ix) => {
            let old = t.entries()[ix].value;
            // (scale1) read without the lock by `lookup_lockfree`
            unsafe { std::sync::atomic::AtomicPtr::from_ptr(&raw mut t.entries_mut()[ix].value).store(value.as_ptr(), Release) };
            t.version = next_version();
            crate::interp::cache::on_dict_write(d, key.as_ptr()); // jit
            drop(g);
            // perf-runtime (D6): lock-free readers may still hold `old`
            unsafe { crate::qsbr::decref_deferred(old) };
        }
        Found::Missing => {
            unsafe { incref(key.as_ptr()) };
            t.insert_new(h, key.as_ptr(), value.as_ptr());
            set_used(d, t.used, key.as_ptr());
            drop(g);
        }
    }
    Ok(())
}

/// Insert a key known not to be present (fresh dict build, e.g. copy). Steals nothing.
pub fn insert_unique(d: &PyObject, key: &PyObject, h: isize, value: &PyObject) {
    let wm = crate::capi::dict::watched_mask(d.as_ptr());
    if wm != 0 {
        crate::capi::dict::notify(d.as_ptr(), wm, crate::capi::dict::EV_ADDED, key.as_ptr(), value.as_ptr());
    }
    let g = ob_lock(d);
    let t = table(d);
    unsafe {
        incref(key.as_ptr());
        incref(value.as_ptr());
    }
    t.insert_new(h, key.as_ptr(), value.as_ptr());
    set_used(d, t.used, key.as_ptr());
    drop(g);
}

/// (perf4) Fill a fresh dict (no other reference yet) with entries whose keys are
/// distinct and absent (borrowed key, its hash, owned value: the value reference is
/// taken over). One lock and one size update for all of them.
pub fn fill_fresh_steal(d: &PyObject, items: &[(*mut PyObject, isize, *mut PyObject)]) {
    let g = ob_lock(d);
    let t = table(d);
    for &(k, h, v) in items {
        unsafe { incref(k) };
        t.insert_new(h, k, v);
    }
    set_used(d, t.used, std::ptr::null_mut());
    drop(g);
}

pub fn set_item_str(d: &PyObject, key: &str, value: &PyObject) -> PyResult<()> {
    let k = crate::types::str::intern_str(key);
    set_item(d, &k, value)
}

/// d.setdefault(key, default) → the value now stored for key.
pub fn setdefault_hash(d: &PyObject, key: &PyObject, h: isize, default: &PyObject) -> PyResult<PyRef> {
    let wm = crate::capi::dict::watched_mask(d.as_ptr());
    if wm != 0 && !contains_hash(d, key, h)? {
        crate::capi::dict::notify(d.as_ptr(), wm, crate::capi::dict::EV_ADDED, key.as_ptr(), default.as_ptr());
    }
    let (g, f) = find(d, key, h)?;
    let t = table(d);
    let r = match f {
        Found::At(ix) => unsafe { PyRef::from_borrowed(t.entries()[ix].value) },
        Found::Missing => {
            unsafe {
                incref(key.as_ptr());
                incref(default.as_ptr());
            }
            t.insert_new(h, key.as_ptr(), default.as_ptr());
            set_used(d, t.used, key.as_ptr());
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
    let wm = crate::capi::dict::watched_mask(d.as_ptr());
    if wm != 0 && contains_hash(d, key, h)? {
        crate::capi::dict::notify(d.as_ptr(), wm, crate::capi::dict::EV_DELETED, key.as_ptr(), std::ptr::null_mut());
    }
    let (g, f) = find(d, key, h)?;
    match f {
        Found::At(ix) => {
            let t = table(d);
            let (k, v) = t.remove(ix);
            set_used(d, t.used, key.as_ptr());
            drop(g);
            unsafe {
                crate::qsbr::decref_deferred(k);
                if crate::qsbr::multi_threaded() {
                    // keep the value alive until racing lock-free readers are done
                    incref(v);
                    crate::qsbr::decref_deferred(v);
                }
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
    set_used(d, t.used, r.0);
    r
}

pub fn clear(d: &PyObject) {
    let wm = crate::capi::dict::watched_mask(d.as_ptr());
    if wm != 0 && dict_len(d) != 0 {
        crate::capi::dict::notify(d.as_ptr(), wm, crate::capi::dict::EV_CLEARED, std::ptr::null_mut(), std::ptr::null_mut());
    }
    let g = ob_lock(d);
    if unsafe { is_shared_empty(d) } {
        return;
    }
    let t = table(d);
    if t.used == 0 && t.nentries == 0 {
        return;
    }
    let mut fresh = DictTable::new(0);
    // (scale1) lock-free readers: the header is updated field by field inside the
    // sequence lock, the new block published through `indices_p`, and the old block
    // outlives them (`old` takes it over and retires it)
    t.wbegin();
    let mut old = DictTable {
        version: t.version,
        layout: t.layout,
        keys_version: t.keys_version,
        entries_p: t.entries_p,
        nentries: t.nentries,
        indices_p: t.indices_p,
        size: t.size,
        cap: t.cap,
        used: t.used,
        fill: t.fill,
        data_bytes: t.data_bytes,
        wseq: std::sync::atomic::AtomicU64::new(0),
        shared_read: std::sync::atomic::AtomicBool::new(t.shared_read.load(Relaxed)),
    };
    t.entries_p = fresh.entries_p;
    t.nentries = 0;
    t.size = fresh.size;
    t.cap = fresh.cap;
    t.used = 0;
    t.fill = 0;
    t.data_bytes = fresh.data_bytes;
    t.version = next_version();
    t.layout = fresh.layout;
    t.keys_version = fresh.keys_version;
    unsafe { std::sync::atomic::AtomicPtr::from_ptr(&raw mut t.indices_p).store(fresh.indices_p, Release) };
    fresh.indices_p = std::ptr::null_mut();
    drop(fresh);
    t.wend();
    set_used(d, 0, std::ptr::null_mut());
    drop(g);
    for e in old.entries().iter() {
        if !e.key.is_null() {
            unsafe {
                crate::qsbr::decref_deferred(e.key);
                crate::qsbr::decref_deferred(e.value);
            }
        }
    }
    old.retire_block();
}

/// Live entry at or after `*pos` (advances `pos` past it).
pub fn next_entry(d: &PyObject, pos: &mut usize) -> Option<(PyRef, PyRef)> {
    let _g = ob_lock(d);
    let t = table(d);
    while *pos < t.nentries {
        let e = t.entries()[*pos];
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
    while *pos < t.nentries {
        let e = t.entries()[*pos];
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
    table(d).nentries
}

/// Snapshot of items (owned).
pub fn items_vec(d: &PyObject) -> Vec<(PyRef, PyRef)> {
    let _g = ob_lock(d);
    let t = table(d);
    let mut v = Vec::with_capacity(t.used);
    for e in t.entries().iter() {
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
    for e in t.entries().iter() {
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
    for e in t.entries().iter() {
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
    for e in t.entries().iter() {
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
        set_used(&n, t.used, std::ptr::null_mut());
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
        let wm = crate::capi::dict::watched_mask(d.as_ptr());
        if wm != 0 {
            crate::capi::dict::notify(d.as_ptr(), wm, crate::capi::dict::EV_CLONED, other.as_ptr(), std::ptr::null_mut());
        }
        let items = items_hash_vec(other);
        let g = ob_lock(d);
        let t = table(d);
        if t.used == 0 {
            if t.size < size_for(items.len()) {
                // (scale1) not `*t = DictTable::new(..)`: lock-free readers may be on
                // the old block, and the header's sequence/sharing state must stay
                t.resize(items.len());
            }
            for (k, v, h) in items {
                t.insert_new(h, k.into_raw(), v.into_raw());
            }
            set_used(d, t.used, std::ptr::null_mut());
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
    let wm = crate::capi::dict::watched_mask(d.as_ptr());
    if wm != 0 {
        let last = { let _g = ob_lock(d); let t = table(d); t.entries()[..t.nentries].iter().rev().find(|e| !e.key.is_null()).map(|e| unsafe { PyRef::from_borrowed(e.key) }) };
        if let Some(k) = last {
            crate::capi::dict::notify(d.as_ptr(), wm, crate::capi::dict::EV_DELETED, k.as_ptr(), std::ptr::null_mut());
        }
    }
    let g = ob_lock(d);
    let t = table(d);
    let mut ix = t.nentries;
    while ix > 0 {
        ix -= 1;
        let e = t.entries()[ix];
        if !e.key.is_null() {
            let (k, v) = t.remove(ix);
            t.nentries = ix;
            set_used(d, t.used, k);
            drop(g);
            if crate::qsbr::multi_threaded() {
                // (scale1) lock-free readers may still be looking at both
                unsafe {
                    incref(k);
                    crate::qsbr::decref_deferred(k);
                    incref(v);
                    crate::qsbr::decref_deferred(v);
                }
            }
            return unsafe { Some((PyRef::from_raw(k), PyRef::from_raw(v))) };
        }
    }
    t.nentries = 0;
    None
}

/// Entry at position `pos` if live: (key, value). Used by iterators.
pub fn entry_at(d: &PyObject, pos: usize) -> Option<(PyRef, PyRef)> {
    let _g = ob_lock(d);
    let t = table(d);
    match t.entries().get(pos) {
        Some(e) if !e.key.is_null() => unsafe { Some((PyRef::from_borrowed(e.key), PyRef::from_borrowed(e.value))) },
        _ => None,
    }
}

/// Approximate memory used by the table (for __sizeof__).
/// Number of hash index slots (a power of two) — used by OrderedDict's
/// size accounting (grind-lib).
pub fn index_slots(d: &PyObject) -> usize {
    let _g = ob_lock(d);
    table(d).size
}

pub fn table_sizeof(d: &PyObject) -> usize {
    let _g = ob_lock(d);
    let t = table(d);
    t.data_bytes + std::mem::size_of::<DictTable>()
}

/// (core6) `dict.__sizeof__` beyond the object itself, in the terms of CPython's keys
/// object for a table of this size (sys.getsizeof is compared with those layouts): none
/// for a dict that never held a key, else the keys header (`n2BI2n`, 32 bytes), one index
/// per slot (1/2/4/8 bytes wide by table size) and the usable entries (16 bytes when every
/// key is an exact str, 24 otherwise).
pub fn model_table_sizeof(d: &PyObject) -> usize {
    let _g = ob_lock(d);
    let t = table(d);
    let entries = if t.nentries == 0 || t.entries_p.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(t.entries_p, t.nentries) } };
    if is_dict_exact(d)
        && d.ob_flags.load(std::sync::atomic::Ordering::Relaxed) & OBF_FROM_INSTANCE != 0
        && t.used <= crate::modules::testinternalcapi::INLINE_VALUES_MAX
        && entries.iter().all(|e| e.key.is_null() || unsafe { crate::types::str::is_str_exact(&*e.key) })
    {
        // (capi3) an instance dict still in the class's attribute layout (CPython's split
        // table): its keys belong to the class; the dict itself holds a values array
        // (one header word plus one slot per value)
        return 8 + 8 * t.used;
    }
    if t.used == 0 && t.nentries == 0 {
        return 0;
    }
    // the size CPython would have grown to for these entries (our growth policy differs)
    let size = t.size.min(size_for(t.nentries.max(1)));
    let width = if size <= 1 << 7 {
        1
    } else if size <= 1 << 15 {
        2
    } else if size <= 1 << 31 {
        4
    } else {
        8
    };
    let unicode = entries.iter().all(|e| e.key.is_null() || unsafe { crate::types::str::is_str_exact(&*e.key) });
    32 + size * width + usable_for(size) * if unicode { 16 } else { 24 }
}
