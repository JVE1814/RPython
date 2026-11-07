//! Sharded reference counts for objects that many threads use at once (scale1).
//!
//! With biased reference counting, a thread that does not own an object changes its
//! count with an atomic RMW on `ob_ref_shared`. For objects every thread touches all
//! the time (functions, methods and descriptors of shared classes, modules, builtin
//! functions, code constants, keys and values of shared read-only tables) that cache
//! line moves between cores on every operation, and N threads run slower than one.
//!
//! Such objects are switched, once they are seen to be hot, to *sharded* counting:
//! * the object gets the `SHARDED` bit in `ob_flags` and a large bias (`BIAS`) in its
//!   shared field, so its fields alone never reach zero;
//! * non-owner increfs and decrefs go to a small per-thread table of pending deltas
//!   (address -> delta), a plain memory add on a thread-private line; the owner keeps
//!   using its local field;
//! * the true count is local + shared - BIAS + the sum of all tables. Tables are
//!   folded into the objects when they fill up, at thread exit, and at every
//!   collection while the world is stopped (`merge_all`);
//! * a sharded object is therefore only ever freed by the collector: GC-tracked ones
//!   by trial deletion (the collector discounts the bias), the others (str, int,
//!   float, bytes, untracked tuples, code) through a registry that the collector
//!   checks for objects whose true count fell to zero. Either way the object returns
//!   to plain counting (`unshard`) and is released after the world restarts.
//!
//! Which objects: exact instances of immutable value types and of the types that
//! make up a program's structure (functions, modules, descriptors, code). Their
//! later release is not observable from Python (no weak references or finalizers on
//! the value types; structure objects are released by the collector in CPython's
//! free-threaded build as well). Mutable containers and user instances keep exact,
//! immediate release. An object becomes sharded when one thread that does not own it
//! drops references to it `HOT` times in a short while (a per-thread direct-mapped
//! candidate cache), so objects merely handed from one thread to another (queues)
//! stay on the plain protocol. Heap types are sharded from creation (per-thread
//! type counts, object.rs), with the same bias and bit.
//!
//! The same design as HotSpot's thread-local allocation of counters / Linux's per-CPU
//! reference counts: cheap thread-private updates, an expensive exact count only when
//! the collector needs it.

use crate::ffi::*;
use std::sync::Mutex;
use std::sync::atomic::Ordering::*;

/// `ob_flags` bit: the object's references are counted per thread.
pub const SHARDED: u16 = 1 << 5;
/// Bias held in `ob_ref_shared` (in references) by a sharded object.
pub const BIAS: isize = crate::object::TYPE_BIAS;

#[inline(always)]
pub fn is_sharded(o: &PyObject) -> bool {
    o.ob_flags.load(Relaxed) & SHARDED != 0
}

// ---------------------------------------------------------------- per-thread tables
//
// Set-associative: an address maps to one bucket of WAYS slots. A slot whose delta is
// zero holds nothing (an incref followed by the matching decref leaves zero), so it
// can be taken by another address at any time; only when all slots of the bucket
// hold pending deltas is one of them folded into its object (one atomic add) to
// make room. Most references taken by a thread are dropped soon after, so the table
// rarely evicts anything, whatever the number of shared objects.

const BBITS: u32 = 8;
const NB: usize = 1 << BBITS;
const WAYS: usize = 4;

#[repr(C, align(64))]
struct Table {
    keys: [[usize; WAYS]; NB],
    vals: [[isize; WAYS]; NB],
    /// Registered in TABLES and not retired: inserts need no further checks.
    ready: bool,
    registered: bool,
    /// The thread has retired its table: later deltas are applied directly.
    dead: bool,
}

#[thread_local]
static mut TABLE: Table = Table { keys: [[0; WAYS]; NB], vals: [[0; WAYS]; NB], ready: false, registered: false, dead: false };

/// Addresses of every live thread's table.
static TABLES: Mutex<Vec<usize>> = Mutex::new(Vec::new());

const HASH_MUL: i32 = 0x2545_F491;
const HASH_SHIFT: u8 = 20;

/// What JIT code needs to update the calling thread's table inline (see
/// `tier1::emit_nonowner_incref`). Offsets are from the thread pointer (fs:0); the
/// table is in the executable's static TLS, so they are the same in every thread.
pub struct JitLayout {
    pub keys: i32,
    /// vals[b][w] - keys[b][w]
    pub vals_delta: i32,
    pub ready: i32,
    pub mul: i32,
    pub shift: u8,
    pub bucket_mask: i32,
    /// log2 of the bytes per bucket row of keys
    pub row_shift: u8,
    pub ways: usize,
}

pub fn jit_layout() -> Option<JitLayout> {
    let base = (&raw const TABLE) as isize - crate::object::thread_id() as isize;
    let keys = base + std::mem::offset_of!(Table, keys) as isize;
    let ready = base + std::mem::offset_of!(Table, ready) as isize;
    if keys.abs() > i32::MAX as isize / 2 || ready.abs() > i32::MAX as isize / 2 {
        return None;
    }
    Some(JitLayout {
        keys: keys as i32,
        vals_delta: (std::mem::offset_of!(Table, vals) - std::mem::offset_of!(Table, keys)) as i32,
        ready: ready as i32,
        mul: HASH_MUL,
        shift: HASH_SHIFT,
        bucket_mask: (NB - 1) as i32,
        row_shift: (WAYS * 8).trailing_zeros() as u8,
        ways: WAYS,
    })
}

#[inline(always)]
fn bucket_of(a: usize) -> usize {
    // a 32-bit multiplier and a shift: JIT code computes the same in three instructions
    (((a >> 4).wrapping_mul(HASH_MUL as usize) >> HASH_SHIFT) & (NB - 1)) as usize
}

#[inline(always)]
unsafe fn apply(a: usize, d: isize) {
    if d != 0 {
        unsafe { (*(a as *mut PyObject)).ob_ref_shared.fetch_add(d << REF_SHARED_SHIFT, Relaxed) };
    }
}

/// Record a change of `d` references to sharded object `op` by this thread.
#[inline]
pub unsafe fn add(op: *mut PyObject, d: isize) {
    unsafe {
        let t = &raw mut TABLE;
        let a = op as usize;
        let b = bucket_of(a);
        let keys = &mut (*t).keys[b];
        let vals = &mut (*t).vals[b];
        for w in 0..WAYS {
            if keys[w] == a {
                vals[w] += d;
                return;
            }
        }
        if !(*t).ready {
            return insert_slow(t, a, d);
        }
        for w in 0..WAYS {
            if vals[w] == 0 {
                keys[w] = a;
                vals[w] = d;
                return;
            }
        }
        evict_insert(t, b, a, d);
    }
}

#[cold]
unsafe fn evict_insert(t: *mut Table, b: usize, a: usize, d: isize) {
    unsafe {
        if crate::scalestats::on() {
            crate::scalestats::SHARDED_FLUSHES.fetch_add(1, Relaxed);
        }
        let w = (a >> 4) & (WAYS - 1);
        apply((*t).keys[b][w], (*t).vals[b][w]);
        (*t).keys[b][w] = a;
        (*t).vals[b][w] = d;
    }
}

#[cold]
unsafe fn insert_slow(t: *mut Table, a: usize, d: isize) {
    unsafe {
        if (*t).dead {
            apply(a, d);
            return;
        }
        if !(*t).registered {
            TABLES.lock().unwrap_or_else(|e| e.into_inner()).push(t as usize);
            (*t).registered = true;
        }
        (*t).ready = true;
        add(a as *mut PyObject, d);
    }
}

/// Fold a table's deltas into the objects (atomically: other threads may change the
/// same fields) and empty it. The bias keeps every object alive meanwhile.
unsafe fn flush_table(t: *mut Table) {
    unsafe {
        for b in 0..NB {
            for w in 0..WAYS {
                let k = (*t).keys[b][w];
                if k != 0 {
                    apply(k, (*t).vals[b][w]);
                    (*t).keys[b][w] = 0;
                    (*t).vals[b][w] = 0;
                }
            }
        }
    }
}

/// Fold every thread's table in. The world must be stopped (or the caller is the
/// only thread).
pub fn merge_all() {
    let g = TABLES.lock().unwrap_or_else(|e| e.into_inner());
    for &t in g.iter() {
        unsafe { flush_table(t as *mut Table) };
    }
}

/// The calling thread is exiting (or its state is retired): fold its table in and
/// unregister it; later deltas of this thread are applied directly. Takes the
/// registry lock, so it cannot overlap a collector's `merge_all`.
pub fn thread_exit() {
    unsafe {
        let t = &raw mut TABLE;
        let mut g = TABLES.lock().unwrap_or_else(|e| e.into_inner());
        flush_table(t);
        if (*t).registered {
            g.retain(|&x| x != t as usize);
            (*t).registered = false;
        }
        (*t).ready = false;
        (*t).dead = true;
    }
}

/// In a forked child: the other threads are gone; their pending deltas still count
/// (references their frames held are leaked, as with every other object).
pub fn after_fork_child() {
    unsafe {
        let me = &raw mut TABLE as usize;
        if let Ok(mut g) = TABLES.try_lock() {
            for &t in g.iter() {
                flush_table(t as *mut Table);
            }
            g.retain(|&x| x == me);
        }
    }
}

/// This thread's pending delta for `op` (for refcount queries).
pub fn own_delta(op: *mut PyObject) -> isize {
    unsafe {
        let t = &raw const TABLE;
        let a = op as usize;
        let b = bucket_of(a);
        for w in 0..WAYS {
            if (*t).keys[b][w] == a {
                return (*t).vals[b][w];
            }
        }
        0
    }
}

/// Fold this thread's pending delta for `op` into its fields and drop the entry.
/// Before an object returns to plain counting no table may hold a delta for it:
/// the collector merges every table when it stops the world, and then calls this
/// for references it took itself during the stop (e.g. `garbage`).
pub fn flush_own_entry(op: *mut PyObject) {
    unsafe {
        let t = &raw mut TABLE;
        let a = op as usize;
        let b = bucket_of(a);
        for w in 0..WAYS {
            if (*t).keys[b][w] == a {
                apply(a, (*t).vals[b][w]);
                (*t).keys[b][w] = 0;
                (*t).vals[b][w] = 0;
            }
        }
    }
}


// ---------------------------------------------------------------- becoming sharded

/// `ob_flags` bits 6-7: saturating count of non-owner decrefs of an eligible object.
const HOT_SHIFT: u16 = 6;
const HOT_MASK: u16 = 3 << HOT_SHIFT;

/// Objects that became sharded (checked by the collector, `dead_untracked`).
static REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static MAKE_LOCK: Mutex<()> = Mutex::new(());

fn eligible_types() -> [*const PyTypeObject; 20] {
    use crate::types::*;
    [
        &raw const float::PyFloat_Type,
        &raw const int::PyLong_Type,
        &raw const str::PyUnicode_Type,
        &raw const bytes::PyBytes_Type,
        &raw const tuple::PyTuple_Type,
        &raw const dict::PyDict_Type,
        &raw const list::PyList_Type,
        &raw const funcobject::PyFunction_Type,
        &raw const module::PyModule_Type,
        &raw const cfunction::PyCFunction_Type,
        &raw const cfunction::PyCMethod_Type,
        &raw const descr::PyMethodDescr_Type,
        &raw const descr::PyClassMethodDescr_Type,
        &raw const descr::PyMemberDescr_Type,
        &raw const descr::PyGetSetDescr_Type,
        &raw const descr::PyWrapperDescr_Type,
        &raw const property::PyProperty_Type,
        &raw const funcobject::PyStaticMethod_Type,
        &raw const funcobject::PyClassMethod_Type,
        &raw const code::PyCode_Type,
    ]
}

#[inline(always)]
fn type_bit(tp: *const PyTypeObject) -> usize {
    ((tp as usize) >> 4).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56
}

/// 256-bit filter over the eligible types (most non-owner decrefs are of other
/// types and are rejected by one bit test).
static FILTER: [std::sync::atomic::AtomicU64; 4] = [const { std::sync::atomic::AtomicU64::new(0) }; 4];
static FILTER_INIT: std::sync::Once = std::sync::Once::new();
static MASK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[inline]
fn eligible(tp: *const PyTypeObject) -> bool {
    FILTER_INIT.call_once(|| {
        // RPY_SHARD_MASK (hex bit mask over `eligible_types()`, 0 = sharding off):
        // for experiments and for narrowing down bugs
        let mask = std::env::var("RPY_SHARD_MASK").ok().and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(u64::MAX);
        MASK.store(mask, Relaxed);
        for (i, t) in eligible_types().into_iter().enumerate() {
            if mask & (1 << i) != 0 {
                let b = type_bit(t);
                FILTER[b >> 6].fetch_or(1 << (b & 63), Relaxed);
            }
        }
    });
    let b = type_bit(tp);
    if FILTER[b >> 6].load(Relaxed) & (1 << (b & 63)) == 0 {
        return false;
    }
    let mask = MASK.load(Relaxed);
    eligible_types().iter().enumerate().any(|(i, &t)| mask & (1 << i) != 0 && std::ptr::eq(t, tp))
}

/// Non-owner decref of `op` (not immortal). Returns true when it was recorded in this
/// thread's table (the object is, or just became, sharded).
#[inline]
pub unsafe fn note_decref(op: *mut PyObject) -> bool {
    unsafe {
        if is_sharded(&*op) {
            add(op, -1);
            return true;
        }
        // Only objects other threads hold references to right now (positive shared
        // count) are candidates; an object handed over by its owner and dropped here
        // (shared count <= 0) is not shared, and counting it would cost a CAS.
        if ((*op).ob_ref_shared.load(Relaxed) >> REF_SHARED_SHIFT) <= 0 {
            return false;
        }
        consider(op)
    }
}

/// An eligible object becomes sharded at its fourth non-owner decref (counted in
/// `ob_flags`, so the working set of shared objects can be of any size); objects
/// handed from one thread to another once never get there.
#[cold]
unsafe fn consider(op: *mut PyObject) -> bool {
    unsafe {
        let o = &*op;
        let tp = o.type_ptr();
        if !eligible(tp) {
            return false;
        }
        // Containers: delaying their release delays their items' release, which
        // may be observable (finalizers, weak references). Only long-lived ones
        // (survived a collection) or, for tuples, untracked ones (atomic items).
        let bits = o.ob_gc_bits.load(Relaxed);
        let old = bits & crate::alloc::GC_BIT_OLD != 0;
        // (stability3) nested functions (closures, callbacks made at run time) keep
        // immediate release, as CPython only defers the counts of functions whose code
        // is not nested: dropping the last reference to a callback must free it then
        // (test_free_threading.test_monitoring checks with a weak reference)
        if std::ptr::eq(tp, &raw const crate::types::funcobject::PyFunction_Type) {
            let co = (*(op as *mut crate::types::funcobject::PyFunctionObject)).func_code;
            if co.is_null() || (*(co as *mut crate::types::code::PyCodeObject)).co_flags & 0x10 != 0 {
                return false;
            }
        }
        if std::ptr::eq(tp, &raw const crate::types::tuple::PyTuple_Type) {
            if bits & crate::alloc::GC_BIT_TRACKED != 0 && !old {
                return false;
            }
        } else if (std::ptr::eq(tp, &raw const crate::types::dict::PyDict_Type)
            || std::ptr::eq(tp, &raw const crate::types::list::PyList_Type))
            && !old
        {
            return false;
        }
        let mut cur = o.ob_flags.load(Relaxed);
        while cur & HOT_MASK != HOT_MASK {
            match o.ob_flags.compare_exchange_weak(cur, cur + (1 << HOT_SHIFT), Relaxed, Relaxed) {
                Ok(_) => return false,
                Err(x) => cur = x,
            }
        }
        if !make_sharded(op) {
            return false;
        }
        add(op, -1);
        true
    }
}

/// Switch `op` to sharded counting. The caller holds a reference.
unsafe fn make_sharded(op: *mut PyObject) -> bool {
    unsafe {
        let o = &*op;
        let _g = MAKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if is_sharded(o) {
            return true;
        }
        if o.is_immortal() || !crate::threads::multi_threaded() {
            return false;
        }
        // the bias first: once the bit is visible, other threads stop updating the
        // fields, and only the bias keeps the owner's last decref from freeing it
        o.ob_ref_shared.fetch_add(BIAS << REF_SHARED_SHIFT, SeqCst);
        o.ob_flags.fetch_or(SHARDED, SeqCst);
        if crate::scalestats::on() {
            crate::scalestats::SHARDED_MADE.fetch_add(1, Relaxed);
        }
        REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).push(op as usize);
        true
    }
}

/// Give heap type `tp` the bit (its bias is added by `assign_type_id`).
pub unsafe fn mark_type(tp: *mut PyObject) {
    unsafe { (*tp).ob_flags.fetch_or(SHARDED, SeqCst) };
}

/// Clear the bit of a heap type that loses its per-thread counting.
pub unsafe fn unmark_type(tp: *mut PyObject) {
    unsafe { (*tp).ob_flags.fetch_and(!SHARDED, SeqCst) };
}

/// Return `op` to plain counting (world stopped, tables merged, the caller holds a
/// reference that keeps it alive): owner's count folded into the shared field,
/// bit and bias removed.
pub unsafe fn unshard(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        if !is_sharded(o) {
            return;
        }
        flush_own_entry(op);
        crate::object::explicit_merge(op);
        o.ob_flags.fetch_and(!(SHARDED | HOT_MASK), SeqCst);
        o.ob_ref_shared.fetch_sub(BIAS << REF_SHARED_SHIFT, SeqCst);
    }
}

/// Collector hook (world stopped, after `merge_all`): sharded objects that trial
/// deletion does not examine and whose count fell to zero. Each is returned to plain
/// counting with one reference, which the caller drops once the world runs again.
pub fn dead_untracked() -> Vec<crate::object::PyRef> {
    let mut out = Vec::new();
    let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    reg.retain(|&a| unsafe {
        let op = a as *mut PyObject;
        let o = &*op;
        if !is_sharded(o) {
            return false; // unsharded by trial deletion
        }
        if o.is_immortal() {
            o.ob_flags.fetch_and(!SHARDED, SeqCst);
            return false;
        }
        if crate::gc::is_tracked(o) {
            // tracked objects are left to trial deletion (a full collection finds
            // them unreachable even when nothing else refers to them)
            return true;
        }
        flush_own_entry(op);
        let local = o.ob_ref_local.load(Relaxed) as isize;
        let shared = o.ob_ref_shared.load(Relaxed) >> REF_SHARED_SHIFT;
        if local + shared - BIAS != 0 {
            // still referenced (a negative count would be an accounting error: keep it)
            return true;
        }
        // dead: plain counting again, holding one reference for the caller
        crate::object::explicit_merge(op);
        o.ob_flags.fetch_and(!(SHARDED | HOT_MASK), SeqCst);
        o.ob_ref_shared.fetch_sub((BIAS - 1) << REF_SHARED_SHIFT, SeqCst);
        out.push(crate::object::PyRef::from_raw(op));
        false
    });
    out
}
