//! Object allocation. Small objects come from the thread-caching page allocator
//! (`tlab.rs`, HotSpot TLAB style); large ones from the system allocator.
//! Objects of types with Py_TPFLAGS_PREHEADER bits carry a pre-header in front:
//!   [managed dict][managed weakref]
//!   PyObject ...
//! GC objects have no list links (perf2): the cycle collector finds small ones by
//! walking the allocator's GC pages (`tlab::for_each_gc_block`) and large ones in the
//! registry kept here ([`for_each_large_gc`]).

use crate::ffi::*;
use crate::object::init_header;
use std::sync::atomic::Ordering::*;

pub const MANAGED: usize = 16;
/// Bits in ob_gc_bits (private to this runtime).
/// The object is GC-capable (allocated on GC pages / in the large registry).
pub const GC_BIT_PREHEADER: u8 = 0x80;
/// The object has the managed dict/weakref pre-header.
pub const GC_BIT_MANAGED: u8 = 0x40;
/// Allocation came from the system allocator (not a tlab page).
pub const GC_BIT_LARGE: u8 = 0x20;
pub const GC_BIT_TRACKED: u8 = 0x01;
pub const GC_BIT_FINALIZED: u8 = 0x02;
/// Survived a collection (old generation).
pub const GC_BIT_OLD: u8 = 0x04;
/// The managed __dict__ was replaced or deleted (no "inline values" any more).
pub const GC_BIT_DICT_DETACHED: u8 = 0x10;

/// (perf5) Header of a block reused for a new object of the same static type (free
/// lists): owned by this thread with one local reference, no shared count, flags
/// and mutex clear, gc bits reduced to the allocation kind (not tracked).
#[inline]
pub unsafe fn reinit_header(op: *mut PyObject) {
    unsafe {
        let kind = (*op).ob_gc_bits.load(Relaxed) & (GC_BIT_PREHEADER | GC_BIT_MANAGED | GC_BIT_LARGE);
        (*op).ob_tid.store(crate::object::thread_id(), Relaxed);
        // ob_flags (u16) | ob_mutex (u8) << 16 | ob_gc_bits (u8) << 24 | ob_ref_local (u32) << 32
        std::ptr::write((op as *mut u64).add(1), ((kind as u64) << 24) | (1u64 << 32));
        (*op).ob_ref_shared.store(0, Relaxed);
    }
}

/// (capi3) The allocation/free fast paths are off: tracemalloc traces or a reference
/// tracer (PyRefTracer_SetTracer) is installed. Recomputed by `refresh_slow_paths`.
pub static SLOW_PATHS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn refresh_slow_paths() {
    let on = crate::modules::_tracemalloc::TRACING.load(SeqCst)
        || crate::capi::pystate::REF_TRACING.load(SeqCst)
        || crate::capi::memdebug::INSTALLED.load(SeqCst);
    SLOW_PATHS.store(on, SeqCst);
}

#[inline(always)]
fn ref_tracing() -> bool {
    crate::capi::pystate::REF_TRACING.load(Relaxed)
}

#[inline]
pub fn type_is_gc(tp: *const PyTypeObject) -> bool {
    unsafe { (*tp).tp_flags & Py_TPFLAGS_HAVE_GC != 0 }
}

/// (pre-header bytes, initial gc bits, allocator kind) for instances of `tp`.
#[inline]
fn preheader_for(tp: *const PyTypeObject) -> (usize, u8, usize) {
    unsafe {
        let f = (*tp).tp_flags;
        if f & Py_TPFLAGS_HAVE_GC == 0 {
            return (0, 0, crate::tlab::KIND_RAW);
        }
        if f & Py_TPFLAGS_PREHEADER != 0 {
            (MANAGED, GC_BIT_PREHEADER | GC_BIT_MANAGED, crate::tlab::KIND_GC_MANAGED)
        } else {
            (0, GC_BIT_PREHEADER, crate::tlab::KIND_GC)
        }
    }
}

#[inline]
fn kind_of_bits(bits: u8) -> usize {
    if bits & GC_BIT_PREHEADER == 0 {
        crate::tlab::KIND_RAW
    } else if bits & GC_BIT_MANAGED != 0 {
        crate::tlab::KIND_GC_MANAGED
    } else {
        crate::tlab::KIND_GC
    }
}

#[inline]
fn raw_obj_alloc(total: usize, kind: usize) -> (*mut u8, u8) {
    if total <= crate::tlab::MAX_SMALL {
        let p = crate::tlab::alloc_kind(total, kind);
        if !p.is_null() {
            return (p, 0);
        }
    }
    (unsafe { libc::calloc(1, total) as *mut u8 }, GC_BIT_LARGE)
}

/// `alloc_object` for non-GC objects whose payload the caller overwrites completely
/// (bytes/str contents): large blocks skip calloc's clearing. Only the object header
/// is initialised. Small blocks come from the (zeroing) tlab as usual.
pub unsafe fn alloc_object_payload_uninit(tp: *mut PyTypeObject, size: usize) -> *mut PyObject {
    unsafe {
        if size <= crate::tlab::MAX_SMALL || (*tp).tp_flags & crate::ffi::Py_TPFLAGS_HAVE_GC != 0 {
            return alloc_object(tp, size);
        }
        if (*tp).tp_flags & crate::ffi::Py_TPFLAGS_READY == 0 {
            ready_lazily(tp);
        }
        let raw = libc::malloc(size) as *mut u8;
        if raw.is_null() {
            return std::ptr::null_mut();
        }
        let op = raw as *mut PyObject;
        init_header(op, tp);
        (*op).ob_gc_bits.store(GC_BIT_LARGE, Relaxed);
        if crate::modules::_tracemalloc::TRACING.load(Relaxed) {
            crate::modules::_tracemalloc::track(op as *const u8, size);
        }
        if ref_tracing() {
            crate::capi::pystate::ref_trace(op, 0);
        }
        op
    }
}

// ---------------------------------------------------------------- large GC objects

/// GC objects from the system allocator (rare: big tuples and the like), keyed by
/// object address.
struct LargeSet(std::collections::HashSet<usize, std::hash::BuildHasherDefault<AddrHasher>>);
#[derive(Default)]
struct AddrHasher(u64);
impl std::hash::Hasher for AddrHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 << 8 | b as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
    }
    fn write_usize(&mut self, v: usize) {
        self.0 = ((v as u64) >> 4).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}
static LARGE_GC: std::sync::Mutex<Option<LargeSet>> = std::sync::Mutex::new(None);

fn large_register(op: *mut PyObject) {
    let mut g = LARGE_GC.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(|| LargeSet(Default::default())).0.insert(op as usize);
}
fn large_unregister(op: *mut PyObject) {
    let mut g = LARGE_GC.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.as_mut() {
        s.0.remove(&(op as usize));
    }
}

/// Call `f` for every GC object living in system-allocator memory.
pub fn for_each_large_gc(mut f: impl FnMut(*mut PyObject)) {
    let g = LARGE_GC.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.as_ref() {
        for &p in s.0.iter() {
            f(p as *mut PyObject);
        }
    }
}

/// Allocate `size` zeroed bytes for an object of type `tp` and initialise its header.
///
/// (perf4) Fast path: a ready type, no tracemalloc, a block from this thread's free
/// list. The allocation kind, pre-header size and initial GC bits follow from two
/// flag bits without branches; the header's flags/mutex/gc-bits/local-refcount words
/// are written with one store. Everything else goes to `alloc_object_slow`.
#[inline]
pub unsafe fn alloc_object(tp: *mut PyTypeObject, size: usize) -> *mut PyObject {
    unsafe {
        let flags = (*tp).tp_flags;
        if flags & Py_TPFLAGS_READY == 0 || size > 512 - MANAGED || SLOW_PATHS.load(Relaxed) {
            return alloc_object_slow(tp, size);
        }
        let gc = ((flags >> 14) & 1) as usize; // Py_TPFLAGS_HAVE_GC
        let managed = gc & (flags & Py_TPFLAGS_PREHEADER != 0) as usize;
        let kind = gc + managed;
        let pre = managed * MANAGED;
        let slot = *crate::tlab::SLOT_OF.get_unchecked(kind).get_unchecked((size + pre + 7) >> 3) as usize;
        let raw = crate::tlab::pop_slot(slot);
        if raw.is_null() {
            return alloc_object_slow(tp, size);
        }
        crate::tlab::zero_slot_small(raw, slot);
        let op = raw.add(pre) as *mut PyObject;
        // gc bits: PREHEADER (0x80) for GC kinds, MANAGED (0x40) with the pre-header
        let bits = ((gc << 7) | (managed << 6)) as u64;
        (*op).ob_tid.store(crate::object::thread_id(), Relaxed);
        // ob_flags (u16) | ob_mutex (u8) << 16 | ob_gc_bits (u8) << 24 | ob_ref_local (u32) << 32
        std::ptr::write((op as *mut u64).add(1), (bits << 24) | (1u64 << 32));
        (*op).ob_type.store(tp, Relaxed);
        if flags & Py_TPFLAGS_HEAPTYPE != 0 {
            // (tail call: keeps the static-type path free of register saves)
            return alloc_heap_type_ref(tp, op);
        }
        op
    }
}

/// The new instance's reference to its heap type (`alloc_object`).
#[inline(never)]
unsafe fn alloc_heap_type_ref(tp: *mut PyTypeObject, op: *mut PyObject) -> *mut PyObject {
    unsafe { crate::object::incref_type(tp) };
    op
}

#[cold]
#[inline(never)]
unsafe fn alloc_object_slow(tp: *mut PyTypeObject, size: usize) -> *mut PyObject {
    unsafe {
        if (*tp).tp_flags & crate::ffi::Py_TPFLAGS_READY == 0 {
            ready_lazily(tp);
        }
        let (pre, bits, kind) = preheader_for(tp);
        let (raw, large) = raw_obj_alloc(size + pre, kind);
        if raw.is_null() {
            return std::ptr::null_mut();
        }
        let op = raw.add(pre) as *mut PyObject;
        init_header(op, tp);
        (*op).ob_gc_bits.store(bits | large, Relaxed);
        if large != 0 && bits != 0 {
            large_register(op);
        }
        if crate::modules::_tracemalloc::TRACING.load(Relaxed) {
            crate::modules::_tracemalloc::track(op as *const u8, size + pre);
        }
        if ref_tracing() {
            crate::capi::pystate::ref_trace(op, 0);
        }
        op
    }
}

#[inline]
pub unsafe fn preheader_size(op: *mut PyObject) -> usize {
    unsafe {
        let b = (*op).ob_gc_bits.load(Relaxed);
        if b & GC_BIT_MANAGED != 0 { MANAGED } else { 0 }
    }
}

#[inline]
pub unsafe fn has_managed(op: *mut PyObject) -> bool {
    unsafe { (*op).ob_gc_bits.load(Relaxed) & GC_BIT_MANAGED != 0 }
}

/// Address of the managed __dict__ slot (only valid if has_managed).
#[inline]
pub unsafe fn managed_dict_slot(op: *mut PyObject) -> *mut *mut PyObject {
    unsafe { (op as *mut u8).sub(MANAGED) as *mut *mut PyObject }
}
#[inline]
pub unsafe fn managed_weakref_slot(op: *mut PyObject) -> *mut *mut PyObject {
    unsafe { (op as *mut u8).sub(8) as *mut *mut PyObject }
}

/// Bytes from the object start to the end of its small-object block (the size class
/// may be larger than requested; that slack is zeroed and usable). For objects from
/// the system allocator: 0 (callers then rely on the requested size).
#[inline]
pub unsafe fn usable_size(op: *mut PyObject) -> usize {
    unsafe {
        if (*op).ob_gc_bits.load(Relaxed) & GC_BIT_LARGE != 0 {
            return 0;
        }
        crate::tlab::small_size_of(alloc_start(op)) - preheader_size(op)
    }
}

/// Raw memory start of an object allocation.
#[inline]
pub unsafe fn alloc_start(op: *mut PyObject) -> *mut u8 {
    unsafe { (op as *mut u8).sub(preheader_size(op)) }
}

#[inline]
pub unsafe fn free_object(op: *mut PyObject) {
    unsafe {
        let bits = (*op).ob_gc_bits.load(Relaxed);
        // (perf4) fast path: a tlab block, no tracemalloc
        if bits & GC_BIT_LARGE == 0 && !SLOW_PATHS.load(Relaxed) {
            if bits & GC_BIT_TRACKED != 0 {
                crate::gc::note_untracked();
            }
            let start = (op as *mut u8).sub(((bits & GC_BIT_MANAGED) as usize) >> 2);
            // a free block on a GC page must not look tracked to heap walks
            (*op).ob_gc_bits.store(0, Relaxed);
            crate::tlab::free_small(start);
            return;
        }
        free_object_slow(op)
    }
}

#[cold]
#[inline(never)]
unsafe fn free_object_slow(op: *mut PyObject) {
    unsafe {
        if ref_tracing() {
            crate::capi::pystate::ref_trace(op, 1);
        }
        // (capi3) an object an extension built in PyObject_Malloc memory (PyObject_Init)
        // under the debug hooks goes back through the object domain
        if crate::capi::memdebug::INSTALLED.load(Relaxed) && crate::capi::memdebug::is_debug_block(op as *const std::ffi::c_void) {
            crate::capi::mem::PyObject_Free(op as *mut std::ffi::c_void);
            return;
        }
        let bits = (*op).ob_gc_bits.load(Relaxed);
        if bits & GC_BIT_TRACKED != 0 {
            crate::gc::untrack(op);
        }
        if crate::modules::_tracemalloc::TRACING.load(Relaxed) {
            crate::modules::_tracemalloc::untrack(op as *const u8);
        }
        let start = alloc_start(op);
        if bits & GC_BIT_LARGE != 0 {
            if bits & GC_BIT_PREHEADER != 0 {
                large_unregister(op);
            }
            libc::free(start as *mut libc::c_void);
        } else {
            // a free block on a GC page must not look tracked to heap walks
            (*op).ob_gc_bits.store(0, Relaxed);
            crate::tlab::free_small(start);
        }
    }
}

pub unsafe fn realloc_object(op: *mut PyObject, new_size: usize) -> *mut PyObject {
    unsafe {
        let nop = realloc_object_impl(op, new_size);
        if !nop.is_null() && crate::modules::_tracemalloc::TRACING.load(Relaxed) {
            let total = new_size + preheader_size(nop);
            crate::modules::_tracemalloc::retrack(op as *const u8, nop as *const u8, total);
        }
        nop
    }
}

unsafe fn realloc_object_impl(op: *mut PyObject, new_size: usize) -> *mut PyObject {
    unsafe {
        let pre = preheader_size(op);
        let bits = (*op).ob_gc_bits.load(Relaxed);
        let start = alloc_start(op);
        let total = new_size + pre;
        let gc = bits & GC_BIT_PREHEADER != 0;
        if bits & GC_BIT_LARGE != 0 {
            let raw = libc::realloc(start as *mut libc::c_void, total) as *mut u8;
            if raw.is_null() {
                return std::ptr::null_mut();
            }
            let nop = raw.add(pre) as *mut PyObject;
            if gc && nop != op {
                large_unregister(op);
                large_register(nop);
            }
            return nop;
        }
        let old = crate::tlab::small_size_of(start);
        if total <= old {
            return op;
        }
        let (raw, large) = raw_obj_alloc(total, kind_of_bits(bits));
        if raw.is_null() {
            return std::ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(start, raw, old);
        (*op).ob_gc_bits.store(0, Relaxed);
        crate::tlab::free_small(start);
        let nop = raw.add(pre) as *mut PyObject;
        // (scale3) the young-buffer entry names the old address: a tracked young object
        // that moved to another small block gets a new entry
        let yb = crate::young::GC_BIT_YOUNG_BUF;
        (*nop).ob_gc_bits.store((bits & !(GC_BIT_LARGE | yb)) | large, Relaxed);
        if bits & yb != 0 && large == 0 {
            (*nop).ob_gc_bits.store((bits & !GC_BIT_LARGE) | large, Relaxed);
            crate::young::push(nop);
        }
        if gc && large != 0 {
            large_register(nop);
        }
        nop
    }
}

pub unsafe fn raw_malloc(n: usize) -> *mut u8 {
    unsafe { libc::malloc(n.max(1)) as *mut u8 }
}
pub unsafe fn raw_calloc(n: usize) -> *mut u8 {
    unsafe { libc::calloc(1, n.max(1)) as *mut u8 }
}
pub unsafe fn raw_realloc(p: *mut u8, n: usize) -> *mut u8 {
    unsafe { libc::realloc(p as *mut libc::c_void, n.max(1)) as *mut u8 }
}
pub unsafe fn raw_free(p: *mut u8) {
    unsafe { libc::free(p as *mut libc::c_void) }
}
pub unsafe extern "C" fn free_object_c(op: *mut PyObject) {
    unsafe {
        let tp = (*op).type_ptr();
        free_object(op);
        // (wheels3) not when an extension's heap-type dealloc called object's tp_dealloc
        // and drops the type reference itself
        if (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE != 0 && !crate::types::typeobject::extension_drops_type_ref(tp) {
            crate::object::decref_type(tp);
        }
    }
}

/// Safety net for static types that were never passed to `type_ready` during startup:
/// ready them the first time an instance is allocated (before the preheader layout is
/// derived from their flags). Readying failures are reported as unraisable.
#[cold]
#[inline(never)]
unsafe fn ready_lazily(tp: *mut PyTypeObject) {
    unsafe {
        if (*tp).tp_flags & crate::ffi::Py_TPFLAGS_READYING != 0 || (*tp).tp_name.is_null() {
            return;
        }
        if crate::types::typeobject::type_ready(tp).is_err() {
            crate::errors::clear_exc();
        }
    }
}

// ---------------------------------------------------------------- variable buffers
//
// Item arrays and hash tables of containers (perf2): small ones come from the tlab
// allocator (kind 0, zeroed), larger ones from the system allocator. The free and
// realloc functions accept either (`tlab::owns`), so storage allocated elsewhere with
// malloc (e.g. by the C API) can be released here too.

/// Zeroed buffer of `n` bytes.
#[inline]
pub unsafe fn buf_calloc(n: usize) -> *mut u8 {
    if n <= crate::tlab::MAX_SMALL {
        let p = crate::tlab::alloc_small(n.max(1));
        if !p.is_null() {
            return p;
        }
    }
    unsafe { libc::calloc(1, n.max(1)) as *mut u8 }
}

/// Resize a buffer from `buf_calloc`/malloc to `n` bytes (contents kept up to the
/// smaller size; growth is not zeroed beyond the old block for system memory).
#[inline]
pub unsafe fn buf_realloc(p: *mut u8, n: usize) -> *mut u8 {
    unsafe {
        if p.is_null() {
            return buf_calloc(n);
        }
        if crate::tlab::owns(p) {
            let old = crate::tlab::small_size_of(p);
            if n <= old {
                return p;
            }
            let q = buf_calloc(n);
            if !q.is_null() {
                std::ptr::copy_nonoverlapping(p, q, old);
                crate::tlab::free_small(p);
            }
            return q;
        }
        libc::realloc(p as *mut libc::c_void, n.max(1)) as *mut u8
    }
}

#[inline]
pub unsafe fn buf_free(p: *mut u8) {
    unsafe {
        if p.is_null() {
            return;
        }
        if crate::tlab::owns(p) {
            crate::tlab::free_small(p);
        } else {
            libc::free(p as *mut libc::c_void);
        }
    }
}
