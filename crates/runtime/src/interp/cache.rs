//! Per-code side tables (HotSpot "MethodData" / constant-pool-cache analogue).
//!
//! The bytecode itself is immutable and shared between threads; everything the
//! interpreter (tier 0) and the JIT tiers learn at run time lives here:
//! * per-instruction inline-cache entries for LoadGlobal / LoadAttr / LoadMethod /
//!   StoreAttr (guards: unique dict version stamps, type version tags),
//! * per-instruction type profiles (one byte, the kinds of operands seen) used by the
//!   optimizing tier to pick speculations,
//! * the tiering state (compiled code pointer, compile lock).
//!
//! Tables are allocated lazily on first execution of a code object and published
//! with release/acquire. Cache entries are small seqlocks: writers bump `seq` to an
//! odd value, write the payload, then publish `seq+2`; readers validate the
//! sequence around their (relaxed) reads. Torn entries are therefore never used.

use crate::ffi::*;
use rpy_compiler::bytecode::Op;
use std::cell::Cell;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering::*};

// ------------------------------------------------------------------ dict versions

static DICT_VERSION: AtomicU64 = AtomicU64::new(1);
const DICT_VERSION_BLOCK: u64 = 1 << 12;

thread_local! {
    static DV: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

/// A fresh, globally unique dict version stamp. Every mutation of a dict (insert,
/// delete, value replacement, clear, rebuild) stores a new stamp, so `(version)`
/// identifies the dict *content*: two dicts never share a non-zero stamp and an
/// unchanged stamp means unchanged content. Threads take blocks of stamps from the
/// global counter to avoid contention (CPython does the same).
#[inline]
pub fn next_dict_version() -> u64 {
    DV.try_with(|c| {
        let (cur, end) = c.get();
        if cur < end {
            c.set((cur + 1, end));
            return cur;
        }
        let start = DICT_VERSION.fetch_add(DICT_VERSION_BLOCK, Relaxed);
        c.set((start + 1, start + DICT_VERSION_BLOCK));
        start
    })
    .unwrap_or_else(|_| DICT_VERSION.fetch_add(1, Relaxed))
}

// ------------------------------------------------------------------ type versions

static TYPE_VERSION: AtomicU32 = AtomicU32::new(1);

#[inline(always)]
fn tag_cell(tp: *mut PyTypeObject) -> &'static AtomicU32 {
    unsafe { &*(&raw mut (*tp).tp_version_tag as *const AtomicU32) }
}

/// Current version tag of a type (0 = none / invalidated).
#[inline(always)]
pub fn type_tag(tp: *mut PyTypeObject) -> u32 {
    tag_cell(tp).load(Acquire)
}

/// Version tag of `tp`, assigning a fresh one if it has none. Returns 0 if the type
/// cannot be cached (not ready, or the tag space is exhausted).
/// `slots::type_modified` resets the tag of the type and all its subclasses whenever
/// a type attribute, base or MRO changes, which invalidates every cache keyed on it.
pub fn assign_type_tag(tp: *mut PyTypeObject) -> u32 {
    let t = type_tag(tp);
    if t != 0 {
        return t;
    }
    unsafe {
        if (*tp).tp_mro.is_null() || (*tp).tp_flags & Py_TPFLAGS_READY == 0 {
            return 0;
        }
    }
    let v = TYPE_VERSION.fetch_add(1, Relaxed);
    if v == 0 || v >= u32::MAX - 1 {
        TYPE_VERSION.store(u32::MAX - 1, Relaxed);
        return 0;
    }
    match tag_cell(tp).compare_exchange(0, v, AcqRel, Acquire) {
        Ok(_) => {
            unsafe { (*tp).tp_flags |= Py_TPFLAGS_VALID_VERSION_TAG };
            v
        }
        Err(cur) => cur,
    }
}

// ------------------------------------------------------------------ type dict watching

/// Mark `tp`'s dict as owned by `tp`: any later mutation of the dict (also by code
/// that writes `tp_dict` directly instead of going through type.__setattr__) calls
/// `type_modified(tp)`, invalidating every cache keyed on the type's version tag.
/// The owner is stored in the dict's (otherwise unused) `_ma_watcher_tag` field.
pub fn watch_type_dict(tp: *mut PyTypeObject) {
    unsafe {
        let d = (*tp).tp_dict;
        if !d.is_null() && crate::types::dict::is_dict(&*d) {
            (*(d as *mut PyDictObject))._ma_watcher_tag = tp as u64;
        }
    }
}

/// The type owning dict `d` is going away.
pub fn unwatch_type_dict(tp: *mut PyTypeObject) {
    unsafe {
        let d = (*tp).tp_dict;
        if !d.is_null() && (*(d as *mut PyDictObject))._ma_watcher_tag == tp as u64 {
            (*(d as *mut PyDictObject))._ma_watcher_tag = 0;
        }
    }
}

/// Called by dict.rs after every mutation.
#[inline(always)]
pub fn on_dict_write(d: &PyObject) {
    let tag = unsafe { (*(d.as_ptr() as *const PyDictObject))._ma_watcher_tag };
    if tag != 0 {
        type_dict_written(tag as *mut PyTypeObject);
    }
}

#[cold]
fn type_dict_written(tp: *mut PyTypeObject) {
    crate::types::slots::type_modified(tp);
}

// ------------------------------------------------------------------ global method cache

/// Global (type version tag, interned name) -> MRO lookup result cache, shared by all
/// threads (CPython's type cache). Entries are seqlocked; the value is borrowed (kept
/// alive by the type's dict while the tag is valid) and may be 0 for "not found".
struct McEntry {
    seq: AtomicU32,
    tag: AtomicU32,
    name: AtomicUsize,
    value: AtomicUsize,
}

const MC_BITS: u32 = 12;
const MC_SIZE: usize = 1 << MC_BITS;

static MC: [McEntry; MC_SIZE] = [const {
    McEntry { seq: AtomicU32::new(0), tag: AtomicU32::new(0), name: AtomicUsize::new(0), value: AtomicUsize::new(0) }
}; MC_SIZE];

#[inline(always)]
fn mc_slot(tag: u32, name: usize) -> &'static McEntry {
    let h = (tag as usize).wrapping_mul(0x9e37_79b9) ^ (name >> 4);
    &MC[h & (MC_SIZE - 1)]
}

/// Cached MRO lookup: Some(result) on a hit.
#[inline]
pub fn mc_lookup(tp: *mut PyTypeObject, name: &PyObject) -> Option<Option<crate::object::PyRef>> {
    let tag = type_tag(tp);
    if tag == 0 {
        return None;
    }
    let e = mc_slot(tag, name.as_ptr() as usize);
    let s1 = e.seq.load(Acquire);
    let t = e.tag.load(Relaxed);
    let n = e.name.load(Relaxed);
    let v = e.value.load(Relaxed);
    std::sync::atomic::fence(Acquire);
    if s1 & 1 != 0 || e.seq.load(Relaxed) != s1 || t != tag || n != name.as_ptr() as usize {
        return None;
    }
    Some(if v == 0 { None } else { Some(unsafe { crate::object::PyRef::from_borrowed(v as *mut PyObject) }) })
}

/// Record a lookup result computed while the type had version tag `tag`.
pub fn mc_store(tag: u32, name: &PyObject, value: Option<&PyObject>) {
    if tag == 0 || !name.is_immortal() {
        return;
    }
    let e = mc_slot(tag, name.as_ptr() as usize);
    let s = e.seq.load(Relaxed);
    if s & 1 != 0 || e.seq.compare_exchange(s, s + 1, Acquire, Relaxed).is_err() {
        return;
    }
    std::sync::atomic::fence(Release);
    e.tag.store(tag, Relaxed);
    e.name.store(name.as_ptr() as usize, Relaxed);
    e.value.store(value.map_or(0, |v| v.as_ptr() as usize), Relaxed);
    e.seq.store(s + 2, Release);
}

// ------------------------------------------------------------------ cache entries

/// Kinds of cache entries (`Entry::kind`).
pub mod kind {
    pub const EMPTY: u32 = 0;
    /// LoadGlobal: a = globals version, b = builtins version, c = value (borrowed)
    pub const GLOBAL: u32 = 1;
    /// LoadAttr/StoreAttr: attribute lives in the instance dict. a = type tag,
    /// b = entry-index hint in the dict.
    pub const INSTANCE_DICT: u32 = 2;
    /// LoadAttr/StoreAttr: __slots__ member at byte offset c. a = type tag.
    pub const SLOT: u32 = 3;
    /// LoadAttr: data descriptor (property, getset, ...) c on the type. a = type tag.
    pub const DATA_DESCR: u32 = 4;
    /// LoadAttr/LoadMethod: class attribute c (non-data descriptor or plain value)
    /// and the instance has no dict slot. a = type tag.
    pub const CLASS_ATTR: u32 = 5;
    /// Like CLASS_ATTR but the instance may have a dict that must not shadow it.
    pub const CLASS_ATTR_SHADOWABLE: u32 = 6;
    /// LoadMethod: c is a method descriptor on the type (pushed as unbound + self);
    /// instance has no dict slot. a = type tag.
    pub const METHOD: u32 = 7;
    /// LoadMethod with an instance dict that must not shadow the name.
    pub const METHOD_SHADOWABLE: u32 = 8;
    /// LoadAttr on a module object: a = module dict version, b = module dict ptr,
    /// c = value.
    pub const MODULE: u32 = 9;
    /// LoadAttr on a class object (metatype `type`): a = class tag, b = class ptr,
    /// c = value (plain, no __get__ binding needed).
    pub const TYPE_ATTR: u32 = 10;
    /// Instruction is not worth caching (megamorphic / unsupported).
    pub const UNCACHEABLE: u32 = 0xff;
}

#[repr(C)]
pub struct Entry {
    pub seq: AtomicU32,
    pub kind: AtomicU32,
    pub a: AtomicU64,
    pub b: AtomicU64,
    pub c: AtomicUsize,
    /// number of misses (to stop re-filling megamorphic sites)
    pub misses: AtomicU32,
    pub _pad: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub kind: u32,
    pub a: u64,
    pub b: u64,
    pub c: usize,
}

const MAX_MISSES: u32 = 64;

impl Entry {
    const fn new() -> Entry {
        Entry {
            seq: AtomicU32::new(0),
            kind: AtomicU32::new(0),
            a: AtomicU64::new(0),
            b: AtomicU64::new(0),
            c: AtomicUsize::new(0),
            misses: AtomicU32::new(0),
            _pad: 0,
        }
    }

    /// Consistent snapshot of the entry, or None if a writer is active.
    #[inline(always)]
    pub fn read(&self) -> Option<Snapshot> {
        let s1 = self.seq.load(Acquire);
        let s = Snapshot {
            kind: self.kind.load(Relaxed),
            a: self.a.load(Relaxed),
            b: self.b.load(Relaxed),
            c: self.c.load(Relaxed),
        };
        std::sync::atomic::fence(Acquire);
        let s2 = self.seq.load(Relaxed);
        if s1 != s2 || s1 & 1 != 0 {
            return None;
        }
        Some(s)
    }

    /// Publish a new entry (dropped silently if another writer is active).
    pub fn write(&self, kind: u32, a: u64, b: u64, c: usize) {
        let s = self.seq.load(Relaxed);
        if s & 1 != 0 {
            return;
        }
        if self.seq.compare_exchange(s, s + 1, Acquire, Relaxed).is_err() {
            return;
        }
        std::sync::atomic::fence(Release);
        self.kind.store(kind, Relaxed);
        self.a.store(a, Relaxed);
        self.b.store(b, Relaxed);
        self.c.store(c, Relaxed);
        self.seq.store(s + 2, Release);
    }

    /// Update only the hint field (b) of an entry, keeping kind/guards.
    pub fn set_hint(&self, b: u64) {
        let s = self.seq.load(Relaxed);
        if s & 1 != 0 || self.seq.compare_exchange(s, s + 1, Acquire, Relaxed).is_err() {
            return;
        }
        std::sync::atomic::fence(Release);
        self.b.store(b, Relaxed);
        self.seq.store(s + 2, Release);
    }

    /// Record a miss; returns false once the site is considered megamorphic.
    #[inline]
    pub fn note_miss(&self) -> bool {
        let m = self.misses.load(Relaxed);
        if m >= MAX_MISSES {
            return false;
        }
        self.misses.store(m + 1, Relaxed);
        true
    }
}

// ------------------------------------------------------------------ type profiles

/// Per-instruction operand profile bits (BinaryOp, CompareOp, BinarySubscr, ...).
pub mod prof {
    pub const INT: u8 = 1;
    pub const FLOAT: u8 = 2;
    pub const STR: u8 = 4;
    pub const LIST: u8 = 8;
    pub const TUPLE: u8 = 16;
    pub const DICT: u8 = 32;
    pub const BIGINT: u8 = 64;
    pub const OTHER: u8 = 128;
}

// ------------------------------------------------------------------ tables

pub struct Tables {
    /// pc -> entry index (u32::MAX: no entry)
    pub index: Box<[u32]>,
    pub entries: Box<[Entry]>,
    /// pc -> profile bits (only meaningful for profiled opcodes)
    pub profile: Box<[AtomicU8]>,
}

fn needs_entry(op: Op) -> bool {
    matches!(op, Op::LoadGlobal | Op::LoadAttr | Op::LoadMethod | Op::StoreAttr)
}

impl Tables {
    fn build(code: &[u32]) -> Tables {
        let mut index = vec![u32::MAX; code.len()].into_boxed_slice();
        let mut n = 0u32;
        for (pc, w) in code.iter().enumerate() {
            let op = Op::from_u8((*w & 0xff) as u8);
            if needs_entry(op) {
                index[pc] = n;
                n += 1;
            }
        }
        let entries: Box<[Entry]> = (0..n).map(|_| Entry::new()).collect();
        let profile: Box<[AtomicU8]> = (0..code.len()).map(|_| AtomicU8::new(0)).collect();
        Tables { index, entries, profile }
    }

    /// Cache entry of the instruction at `pc` (must be a cached opcode).
    #[inline(always)]
    pub fn entry(&self, pc: usize) -> &Entry {
        unsafe {
            let i = *self.index.get_unchecked(pc);
            debug_assert!(i != u32::MAX);
            self.entries.get_unchecked(i as usize)
        }
    }

    #[inline(always)]
    pub fn record(&self, pc: usize, bits: u8) {
        let p = unsafe { self.profile.get_unchecked(pc) };
        let old = p.load(Relaxed);
        if old | bits != old {
            p.store(old | bits, Relaxed);
        }
    }
}

/// Tiering state of a code object.
pub mod tier {
    pub const INTERP: u8 = 0;
    pub const COMPILING: u8 = 1;
    pub const COMPILED: u8 = 2;
    /// compilation failed / not compilable: stay in the interpreter
    pub const FAILED: u8 = 3;
}

pub struct CodeCaches {
    tables: AtomicPtr<Tables>,
    /// Installed machine code (`jit::Compiled`), published with Release.
    pub jit: AtomicPtr<crate::interp::jit::Compiled>,
    /// tier::* state; also the per-function compilation lock (CAS INTERP->COMPILING).
    pub tier_state: AtomicU8,
}

impl CodeCaches {
    pub fn new(ninstr: usize) -> CodeCaches {
        let _ = ninstr;
        CodeCaches { tables: AtomicPtr::new(std::ptr::null_mut()), jit: AtomicPtr::new(std::ptr::null_mut()), tier_state: AtomicU8::new(0) }
    }

    /// The side tables, allocating them on first use.
    #[inline]
    pub fn tables(&self, code: &[u32]) -> &Tables {
        let p = self.tables.load(Acquire);
        if !p.is_null() {
            return unsafe { &*p };
        }
        self.alloc_tables(code)
    }

    #[cold]
    fn alloc_tables(&self, code: &[u32]) -> &Tables {
        let t = Box::into_raw(Box::new(Tables::build(code)));
        match self.tables.compare_exchange(std::ptr::null_mut(), t, AcqRel, Acquire) {
            Ok(_) => unsafe { &*t },
            Err(cur) => {
                unsafe { drop(Box::from_raw(t)) };
                unsafe { &*cur }
            }
        }
    }

    /// Tables if already allocated.
    #[inline]
    pub fn tables_opt(&self) -> Option<&Tables> {
        let p = self.tables.load(Acquire);
        if p.is_null() { None } else { Some(unsafe { &*p }) }
    }
}

impl Drop for CodeCaches {
    fn drop(&mut self) {
        let p = *self.tables.get_mut();
        if !p.is_null() {
            unsafe { drop(Box::from_raw(p)) };
        }
        let j = *self.jit.get_mut();
        if !j.is_null() {
            crate::interp::jit::release_compiled(j);
        }
    }
}
