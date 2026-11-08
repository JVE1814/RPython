//! Small-object allocator (HotSpot TLAB / tcmalloc style).
//!
//! * Memory comes from 4 MiB segments (mmap), split into 64 KiB pages.
//! * Each page serves one size class of one *kind*; the page header (first 64 bytes)
//!   records both, so `free` needs only the pointer (page = ptr & !(PAGE-1)).
//!   Kinds (perf2): 0 = everything that is not a GC object (also table storage and
//!   PyObject_Malloc memory), 1 = GC objects without a pre-header, 2 = GC objects
//!   with the 16-byte managed dict/weakref pre-header. Keeping GC objects on pages of
//!   their own, with the object at a fixed offset in each block, lets the cycle
//!   collector find every GC object by walking those pages ([`for_each_gc_block`]),
//!   so GC objects need no list links.
//! * Each thread keeps a free list per (kind, size class) (no atomics on the fast
//!   path). Frees from any thread go to the freeing thread's cache (blocks are
//!   interchangeable). Overfull caches spill batches to a global depot; empty caches
//!   refill from it, and only then carve fresh blocks. Pages are carved lazily, a few
//!   KiB at a time (untouched memory stays unmapped), and the page header records the
//!   carved extent for heap walks.
//! * Blocks larger than the biggest class are served by the system allocator.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::*};

pub const PAGE: usize = 64 * 1024;
const SEGMENT: usize = 4 * 1024 * 1024;
const HEADER: usize = 64;
const MAGIC: u32 = 0x5250_5947; // "RPYG"

/// Block sizes. The 8-byte steps below 96 are used by GC objects only (instances with
/// inline attributes, tuples, lists): other allocations are rounded to 16 bytes, which
/// PyObject_Malloc callers may rely on for alignment.
/// (mem1) From 384 bytes on the steps are 32 bytes up to 1 KiB and 128 bytes up to 2 KiB
/// (were 64 and 256): at most ~11% of a block is rounding (was up to 25%, e.g. a 100-item
/// list's 800-byte item array took 832, a 1040-byte buffer 1280). A class that is used
/// costs only the pages it carves, so the extra classes cost no memory when unused.
pub const CLASSES: [usize; 50] = [
    16, 32, 40, 48, 56, 64, 72, 80, 88, 96, 112, 128, 144, 160, 176, 192, 224, 256, 288, 320, 352, 384, 416, 448, 480, 512, 544, 576, 608, 640,
    672, 704, 736, 768, 800, 832, 864, 896, 928, 960, 992, 1024, 1152, 1280, 1408, 1536, 1664, 1792, 1920, 2048,
];
pub const MAX_SMALL: usize = 2048;
const NCLASSES: usize = CLASSES.len();
/// Allocation kinds (see the module docs).
pub const KIND_RAW: usize = 0;
pub const KIND_GC: usize = 1;
pub const KIND_GC_MANAGED: usize = 2;
const NKINDS: usize = 3;
/// Free lists: one per (kind, class); index `kind * NCLASSES + class`.
const NSLOTS: usize = NKINDS * NCLASSES;
// slots are stored as u8 (SLOT_OF)
const _: () = assert!(NSLOTS <= 256);
/// Pre-header bytes in front of the object in a block of each kind.
const KIND_PRE: [usize; NKINDS] = [0, 0, 16];

/// size -> class index lookup for sizes up to 2048 in 8-byte steps.
static CLASS_OF: [u8; MAX_SMALL / 8 + 1] = {
    let mut t = [0u8; MAX_SMALL / 8 + 1];
    let mut i = 0;
    while i <= MAX_SMALL / 8 {
        let size = i * 8;
        let mut c = 0;
        while CLASSES[c] < size {
            c += 1;
        }
        t[i] = c as u8;
        i += 1;
    }
    t
};

/// (perf4) Free-list slot for (kind, size in 8-byte units rounded up): kind 0 rounds
/// to 16 bytes, the GC kinds use the 8-byte classes. Saves the rounding and the
/// `kind * NCLASSES` arithmetic on the allocation fast path.
pub static SLOT_OF: [[u8; MAX_SMALL / 8 + 1]; NKINDS] = {
    let mut t = [[0u8; MAX_SMALL / 8 + 1]; NKINDS];
    let mut k = 0;
    while k < NKINDS {
        let mut i = 0;
        while i <= MAX_SMALL / 8 {
            let size = if k == KIND_RAW { (i * 8 + 15) & !15 } else { i * 8 };
            t[k][i] = (CLASS_OF[size / 8] as usize + k * NCLASSES) as u8;
            i += 1;
        }
        k += 1;
    }
    t
};

/// Block size of each free-list slot.
pub static SLOT_SIZE: [u16; NSLOTS] = {
    let mut t = [0u16; NSLOTS];
    let mut s = 0;
    while s < NSLOTS {
        t[s] = CLASSES[s % NCLASSES] as u16;
        s += 1;
    }
    t
};

#[repr(C)]
struct PageHeader {
    magic: u32,
    /// Free-list slot: `kind * NCLASSES + class` (read by JIT code as the class).
    class: u32,
    block: usize,
    /// Address of the first block.
    first: usize,
    /// End of the carved part (blocks at or beyond it were never handed out).
    carved: AtomicUsize,
}

#[inline(always)]
pub fn class_for(size: usize) -> Option<usize> {
    if size > MAX_SMALL {
        return None;
    }
    Some(CLASS_OF[((size + 15) & !15) / 8] as usize)
}

struct Depot {
    /// batches of free blocks per slot
    batches: Vec<Vec<Vec<usize>>>,
    /// Uncarved block ranges `(next, end)` handed back by exiting threads, per slot.
    partial: Vec<Vec<(usize, usize)>>,
    /// current segment carve state
    seg_cur: usize,
    seg_end: usize,
}

/// Number of free-block batches in the depot per slot (lets refills skip the depot
/// lock when there is nothing to take).
static DEPOT_BATCHES: [AtomicUsize; NSLOTS] = [const { AtomicUsize::new(0) }; NSLOTS];

/// Every segment ever mapped (append-only; read by heap walks without the depot lock).
const MAX_SEGS: usize = 1 << 16;
static SEGS: [AtomicUsize; MAX_SEGS] = [const { AtomicUsize::new(0) }; MAX_SEGS];
static NSEGS: AtomicUsize = AtomicUsize::new(0);

#[inline(always)]
fn page_of(p: usize) -> *mut PageHeader {
    (p & !(PAGE - 1)) as *mut PageHeader
}

/// A fresh page for free-list `slot`: writes the header and returns the block range
/// `(first, end)`. Blocks are not touched here; threads carve them lazily (a few KiB at
/// a time), so a size class used for only a handful of objects costs a page or two of
/// resident memory, not 64 KiB.
fn new_page(d: &mut Depot, slot: usize) -> Option<(usize, usize)> {
    if d.seg_cur + PAGE > d.seg_end {
        let n = NSEGS.load(Relaxed);
        if n >= MAX_SEGS {
            return None;
        }
        unsafe {
            // over-allocate to guarantee SEGMENT alignment (so `owns` is a bitmap lookup)
            let raw = libc::mmap(
                std::ptr::null_mut(),
                2 * SEGMENT,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            if raw == libc::MAP_FAILED {
                return None;
            }
            let start = (raw as usize + SEGMENT - 1) & !(SEGMENT - 1);
            if start > raw as usize {
                libc::munmap(raw, start - raw as usize);
            }
            let (tail, end) = (start + SEGMENT, raw as usize + 2 * SEGMENT);
            if end > tail {
                libc::munmap(tail as *mut libc::c_void, end - tail);
            }
            let g = start / SEGMENT;
            SEGMENT_MAP[g / 64].fetch_or(1 << (g % 64), Release);
            SEGS[n].store(start, Release);
            NSEGS.store(n + 1, Release);
            d.seg_cur = start;
            d.seg_end = start + SEGMENT;
        }
    }
    let page = d.seg_cur;
    d.seg_cur += PAGE;
    let block = CLASSES[slot % NCLASSES];
    let first = page + HEADER.max(block.min(HEADER));
    let first = (first + 15) & !15;
    let n = (page + PAGE - first) / block;
    unsafe {
        let h = page as *mut PageHeader;
        (*h).class = slot as u32;
        (*h).block = block;
        (*h).first = first;
        (*h).carved.store(first, Release);
        // magic last: a walker only trusts pages with a complete header
        std::ptr::write_volatile(&raw mut (*h).magic, MAGIC);
    }
    Some((first, first + n * block))
}

static DEPOT: Mutex<Option<Depot>> = Mutex::new(None);

const BATCH: usize = 64;
const CACHE_LIMIT: usize = 256;
/// Bytes of fresh blocks linked into a thread's free list per refill.
const CARVE_BYTES: usize = 2048;

fn new_depot() -> Depot {
    Depot {
        batches: (0..NSLOTS).map(|_| Vec::new()).collect(),
        partial: (0..NSLOTS).map(|_| Vec::new()).collect(),
        seg_cur: 0,
        seg_end: 0,
    }
}

/// Per-thread cache: one intrusive free list per slot (the first word of a free
/// block links to the next one), plus the uncarved rest of the page the thread is
/// currently carving for each slot. `#[thread_local]` gives direct fs-relative access
/// without lazy-init checks; a separate guard returns the blocks to the depot when the
/// thread exits. Slots 0..NCLASSES are kind 0, so JIT code indexes `heads`/`counts`
/// with the plain class (allocation) or the page header's slot (free).
struct ThreadCache {
    heads: [usize; NSLOTS],
    counts: [u32; NSLOTS],
    carve_next: [usize; NSLOTS],
    carve_end: [usize; NSLOTS],
    registered: bool,
    dead: bool,
    /// (perf4) registered, not exiting and not in double-free checking mode: a free
    /// may push onto the free list without further tests.
    fast_free: bool,
}

#[thread_local]
static mut TC: ThreadCache = ThreadCache {
    heads: [0; NSLOTS],
    counts: [0; NSLOTS],
    carve_next: [0; NSLOTS],
    carve_end: [0; NSLOTS],
    registered: false,
    dead: false,
    fast_free: false,
};

struct ExitGuard;
impl Drop for ExitGuard {
    fn drop(&mut self) {
        unsafe {
            let tc = &mut *(&raw mut TC);
            tc.dead = true;
            tc.fast_free = false;
            let mut g = DEPOT.lock().unwrap_or_else(|e| e.into_inner());
            let d = g.get_or_insert_with(new_depot);
            for c in 0..NSLOTS {
                let mut v = Vec::with_capacity(tc.counts[c] as usize);
                let mut p = tc.heads[c];
                while p != 0 {
                    v.push(p);
                    p = *(p as *const usize);
                }
                tc.heads[c] = 0;
                tc.counts[c] = 0;
                if !v.is_empty() {
                    d.batches[c].push(v);
                    DEPOT_BATCHES[c].fetch_add(1, Relaxed);
                }
                if tc.carve_next[c] < tc.carve_end[c] {
                    d.partial[c].push((tc.carve_next[c], tc.carve_end[c]));
                }
                tc.carve_next[c] = 0;
                tc.carve_end[c] = 0;
            }
        }
    }
}

thread_local! {
    static GUARD: ExitGuard = const { ExitGuard };
}

#[cold]
fn register(tc: &mut ThreadCache) {
    tc.registered = true;
    let guarded = GUARD.try_with(|_| ()).is_ok();
    tc.fast_free = guarded && !tc.dead && !checking();
}

/// Take a batch of freed blocks from the depot, or else a carving range.
fn depot_take(d: &mut Depot, slot: usize) -> Result<Vec<usize>, Option<(usize, usize)>> {
    if let Some(b) = d.batches[slot].pop() {
        DEPOT_BATCHES[slot].fetch_sub(1, Relaxed);
        return Ok(b);
    }
    if let Some(r) = d.partial[slot].pop() {
        return Err(Some(r));
    }
    Err(new_page(d, slot))
}

/// Mark `[.., end)` of the page containing `block` as carved.
#[inline]
fn note_carved(block: usize, end: usize) {
    unsafe { (*page_of(block)).carved.fetch_max(end, Release) };
}

#[cold]
fn refill(slot: usize, tc: &mut ThreadCache) {
    if !tc.registered {
        register(tc);
    }
    // freed blocks parked in the depot first (reuses memory that is already resident)
    if DEPOT_BATCHES[slot].load(Relaxed) != 0 || tc.carve_next[slot] >= tc.carve_end[slot] {
        let got = {
            let mut g = DEPOT.lock().unwrap_or_else(|e| e.into_inner());
            let d = g.get_or_insert_with(new_depot);
            if tc.carve_next[slot] < tc.carve_end[slot] && d.batches[slot].is_empty() {
                Err(None)
            } else {
                depot_take(d, slot)
            }
        };
        match got {
            Ok(blocks) => {
                for p in blocks {
                    unsafe { *(p as *mut usize) = tc.heads[slot] };
                    tc.heads[slot] = p;
                    tc.counts[slot] += 1;
                }
                return;
            }
            Err(Some((a, b))) => {
                if tc.carve_next[slot] < tc.carve_end[slot] {
                    // keep the current range; hand the new one back
                    let mut g = DEPOT.lock().unwrap_or_else(|e| e.into_inner());
                    g.get_or_insert_with(new_depot).partial[slot].push((a, b));
                } else {
                    tc.carve_next[slot] = a;
                    tc.carve_end[slot] = b;
                }
            }
            Err(None) => {}
        }
    }
    // carve a few KiB of fresh blocks, lowest address first
    let size = CLASSES[slot % NCLASSES];
    let next = tc.carve_next[slot];
    let end = tc.carve_end[slot];
    if next >= end {
        return; // out of memory
    }
    let n = (CARVE_BYTES / size).max(1).min((end - next) / size);
    note_carved(next, next + n * size);
    let mut p = next + (n - 1) * size;
    loop {
        unsafe { *(p as *mut usize) = tc.heads[slot] };
        tc.heads[slot] = p;
        tc.counts[slot] += 1;
        if p == next {
            break;
        }
        p -= size;
    }
    tc.carve_next[slot] = next + n * size;
}

#[cold]
fn spill(slot: usize, tc: &mut ThreadCache) {
    let mut batch = Vec::with_capacity(BATCH);
    for _ in 0..BATCH {
        let p = tc.heads[slot];
        if p == 0 {
            break;
        }
        tc.heads[slot] = unsafe { *(p as *const usize) };
        tc.counts[slot] -= 1;
        batch.push(p);
    }
    let mut g = DEPOT.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(new_depot).batches[slot].push(batch);
    DEPOT_BATCHES[slot].fetch_add(1, Relaxed);
}

#[inline(always)]
unsafe fn zero_block(p: *mut u8, size: usize) {
    unsafe {
        // (perf4) inline wide stores (block sizes are multiples of 8), opaque to the
        // optimiser so that LLVM does not turn them back into a memset call of
        // variable size (it merged the previous per-size `write_bytes` arms into
        // exactly that).
        if size > 512 {
            std::ptr::write_bytes(p, 0, size);
            return;
        }
        let mut off = 0;
        while off + 32 <= size {
            store_zero::<32>(p.add(off));
            off += 32;
        }
        if off + 16 <= size {
            store_zero::<16>(p.add(off));
            off += 16;
        }
        if off < size {
            store_zero::<8>(p.add(off));
        }
    }
}

#[inline(always)]
unsafe fn store_zero<const N: usize>(p: *mut u8) {
    // (blocks are only 8-byte aligned: unaligned vector stores)
    #[cfg(all(target_arch = "x86_64", target_feature = "avx"))]
    unsafe {
        let z = std::arch::x86_64::_mm256_setzero_si256();
        match N {
            32 => std::arch::asm!("vmovdqu ymmword ptr [{p}], {z}", p = in(reg) p, z = in(ymm_reg) z, options(nostack, preserves_flags)),
            16 => std::arch::asm!("vmovdqu xmmword ptr [{p}], {z}", p = in(reg) p, z = in(xmm_reg) std::arch::x86_64::_mm256_castsi256_si128(z), options(nostack, preserves_flags)),
            _ => std::ptr::write_volatile(p as *mut u64, 0),
        }
    }
    #[cfg(not(all(target_arch = "x86_64", target_feature = "avx")))]
    unsafe {
        for i in 0..N / 8 {
            std::ptr::write_volatile((p as *mut u64).add(i), 0);
        }
    }
}

/// Allocate a zeroed small block (size <= MAX_SMALL) of kind 0. Null on OOM.
#[inline]
pub fn alloc_small(size: usize) -> *mut u8 {
    alloc_kind(size, KIND_RAW)
}

/// Allocate a zeroed small block (size <= MAX_SMALL) of the given kind. Null on OOM.
#[inline(always)]
pub fn alloc_kind(size: usize, kind: usize) -> *mut u8 {
    let c = SLOT_OF[kind][(size + 7) / 8] as usize;
    unsafe {
        let p = pop_slot(c);
        if !p.is_null() {
            zero_block(p, SLOT_SIZE[c] as usize);
            return p;
        }
        alloc_slot_slow(c)
    }
}

/// (perf4) Pop a block off this thread's free list for `slot` (not zeroed); null when
/// the list is empty (the caller then takes `alloc_slot_slow`).
#[inline(always)]
pub unsafe fn pop_slot(slot: usize) -> *mut u8 {
    unsafe {
        let tc = &mut *(&raw mut TC);
        let p = *tc.heads.get_unchecked(slot);
        if p == 0 {
            return std::ptr::null_mut();
        }
        *tc.heads.get_unchecked_mut(slot) = *(p as *const usize);
        *tc.counts.get_unchecked_mut(slot) -= 1;
        p as *mut u8
    }
}

/// Zero a block of free-list slot `slot`.
#[inline(always)]
pub unsafe fn zero_slot(p: *mut u8, slot: usize) {
    unsafe { zero_block(p, *SLOT_SIZE.get_unchecked(slot) as usize) }
}

/// `zero_slot` for blocks of at most 512 bytes (no library call: callers stay leaf
/// functions).
#[inline(always)]
pub unsafe fn zero_slot_small(p: *mut u8, slot: usize) {
    unsafe {
        let size = *SLOT_SIZE.get_unchecked(slot) as usize;
        let mut off = 0;
        while off + 32 <= size {
            store_zero::<32>(p.add(off));
            off += 32;
        }
        if off + 16 <= size {
            store_zero::<16>(p.add(off));
            off += 16;
        }
        if off < size {
            store_zero::<8>(p.add(off));
        }
    }
}

/// Refill path of `alloc_kind`: a zeroed block of `slot`, null on OOM.
#[cold]
#[inline(never)]
pub fn alloc_slot_slow(c: usize) -> *mut u8 {
    unsafe {
        let tc = &mut *(&raw mut TC);
        let mut p = tc.heads[c];
        if p == 0 {
            if tc.dead {
                return alloc_from_depot(c);
            }
            refill(c, tc);
            p = tc.heads[c];
            if p == 0 {
                return std::ptr::null_mut();
            }
        }
        tc.heads[c] = *(p as *const usize);
        tc.counts[c] -= 1;
        zero_block(p as *mut u8, CLASSES[c % NCLASSES]);
        p as *mut u8
    }
}

#[cold]
fn alloc_from_depot(c: usize) -> *mut u8 {
    // thread is exiting: take directly from the depot
    let mut g = DEPOT.lock().unwrap_or_else(|e| e.into_inner());
    let d = g.get_or_insert_with(new_depot);
    let size = CLASSES[c % NCLASSES];
    let p = match depot_take(d, c) {
        Ok(mut v) => {
            let p = v.pop().unwrap_or(0);
            if !v.is_empty() {
                d.batches[c].push(v);
                DEPOT_BATCHES[c].fetch_add(1, Relaxed);
            }
            p
        }
        Err(Some((a, b))) => {
            note_carved(a, a + size);
            if a + size < b {
                d.partial[c].push((a + size, b));
            }
            a
        }
        Err(None) => 0,
    };
    if p != 0 {
        unsafe { std::ptr::write_bytes(p as *mut u8, 0, size) };
    }
    p as *mut u8
}

/// Free-list slot (kind * classes + class) of a small block.
#[inline]
pub fn small_class_of(p: *mut u8) -> usize {
    unsafe {
        let h = page_of(p as usize);
        debug_assert_eq!((*h).magic, MAGIC);
        (*h).class as usize
    }
}

/// Is `p` (an address inside a small block) on a GC-object page?
#[inline]
pub fn is_gc_block(p: *mut u8) -> bool {
    small_class_of(p) >= NCLASSES
}

#[inline]
pub fn small_size_of(p: *mut u8) -> usize {
    CLASSES[small_class_of(p) % NCLASSES]
}

/// Debugging aid (stability workstream): with `RPY_TLAB_CHECK=1` in the environment,
/// freed blocks are stamped in their second word and freeing a stamped block aborts
/// ("double free"), instead of silently linking a cycle into the free list (which shows
/// up much later as heap corruption or a thread-exit loop). Use with RPYTHON_JIT=0:
/// the JIT's inline free path does not check.
static CHECK_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
const FREED_STAMP: usize = 0xdead_f4ee_dead_f4ee;

#[cold]
fn check_mode_init() -> bool {
    let on = std::env::var_os("RPY_TLAB_CHECK").is_some_and(|v| v != "0");
    CHECK_MODE.store(if on { 2 } else { 1 }, Relaxed);
    on
}

#[inline(always)]
fn checking() -> bool {
    match CHECK_MODE.load(Relaxed) {
        1 => false,
        2 => true,
        _ => check_mode_init(),
    }
}

#[cold]
#[inline(never)]
fn check_free(p: *mut u8) {
    unsafe {
        let w = (p as *mut usize).add(1);
        if *w == FREED_STAMP {
            let msg = format!("rpython: double free of small block {:p} (class {})\n", p, small_class_of(p));
            libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len());
            libc::abort();
        }
        *w = FREED_STAMP;
    }
}

/// (perf5) This thread may keep freed objects for reuse (registered, not exiting, no
/// heap checking): object free lists use it.
#[inline]
pub fn fast_free_ok() -> bool {
    unsafe { (*(&raw const TC)).fast_free }
}

/// Free a block returned by `alloc_small` / `alloc_kind`.
#[inline]
pub fn free_small(p: *mut u8) {
    let c = small_class_of(p);
    unsafe {
        let tc = &mut *(&raw mut TC);
        if tc.fast_free {
            *(p as *mut usize) = *tc.heads.get_unchecked(c);
            *tc.heads.get_unchecked_mut(c) = p as usize;
            let n = *tc.counts.get_unchecked(c) + 1;
            *tc.counts.get_unchecked_mut(c) = n;
            if n as usize > CACHE_LIMIT {
                spill(c, tc);
            }
            return;
        }
    }
    free_small_slow(p, c)
}

#[cold]
#[inline(never)]
fn free_small_slow(p: *mut u8, c: usize) {
    if checking() {
        check_free(p);
    }
    unsafe {
        let tc = &mut *(&raw mut TC);
        if tc.dead {
            let mut g = DEPOT.lock().unwrap_or_else(|e| e.into_inner());
            g.get_or_insert_with(new_depot).batches[c].push(vec![p as usize]);
            DEPOT_BATCHES[c].fetch_add(1, Relaxed);
            return;
        }
        if !tc.registered {
            register(tc);
        }
        *(p as *mut usize) = tc.heads[c];
        tc.heads[c] = p as usize;
        tc.counts[c] += 1;
        if tc.counts[c] as usize > CACHE_LIMIT {
            spill(c, tc);
        }
    }
}

/// Call `f(object)` for every block of the GC kinds that was ever handed out, with
/// the object address (block + pre-header). Free blocks are included: callers check
/// the object's GC bits (freed GC objects have them cleared). Only meaningful while
/// no other thread allocates (the world is stopped).
pub unsafe fn for_each_gc_block(mut f: impl FnMut(*mut crate::ffi::PyObject)) {
    let n = NSEGS.load(Acquire);
    for s in 0..n {
        let seg = SEGS[s].load(Acquire);
        let mut page = seg;
        while page < seg + SEGMENT {
            let h = page as *const PageHeader;
            unsafe {
                if std::ptr::read_volatile(&(*h).magic) != MAGIC {
                    // pages of a segment are handed out in address order
                    break;
                }
                let kind = (*h).class as usize / NCLASSES;
                if kind != KIND_RAW {
                    let pre = KIND_PRE[kind];
                    let block = (*h).block;
                    let end = (*h).carved.load(Acquire);
                    let mut b = (*h).first;
                    while b + block <= end {
                        f((b + pre) as *mut crate::ffi::PyObject);
                        b += block;
                    }
                }
            }
            page += PAGE;
        }
    }
}

/// (mem1) Per free-list slot: (kind, block size, pages, carved bytes, blocks cached free by
/// the calling thread). For `sys._debugmallocstats()`; a racy snapshot.
pub fn slot_stats() -> Vec<(usize, usize, usize, usize, usize)> {
    let mut pages = [0usize; NSLOTS];
    let mut carved = [0usize; NSLOTS];
    let n = NSEGS.load(Acquire);
    for s in 0..n {
        let seg = SEGS[s].load(Acquire);
        let mut page = seg;
        while page < seg + SEGMENT {
            let h = page as *const PageHeader;
            unsafe {
                if std::ptr::read_volatile(&(*h).magic) != MAGIC {
                    break;
                }
                let slot = (*h).class as usize;
                pages[slot] += 1;
                carved[slot] += (*h).carved.load(Acquire) - (*h).first;
            }
            page += PAGE;
        }
    }
    let tc = unsafe { &*(&raw const TC) };
    (0..NSLOTS)
        .filter(|&s| pages[s] != 0)
        .map(|s| (s / NCLASSES, CLASSES[s % NCLASSES], pages[s], carved[s], tc.counts[s] as usize))
        .collect()
}

/// Layout of the per-thread cache for JIT-generated inline allocation/free (jit
/// workstream). `tls_offset` is the cache address minus the thread pointer (fs:0);
/// it is the same for every thread (static TLS of the executable).
pub struct JitLayout {
    pub tls_offset: isize,
    pub heads: usize,
    pub counts: usize,
    pub page_class: usize,
    pub limit: usize,
}

pub fn jit_layout() -> JitLayout {
    JitLayout {
        tls_offset: (&raw const TC) as isize - crate::object::thread_id() as isize,
        heads: std::mem::offset_of!(ThreadCache, heads),
        counts: std::mem::offset_of!(ThreadCache, counts),
        page_class: std::mem::offset_of!(PageHeader, class),
        limit: CACHE_LIMIT,
    }
}

/// Size class index (kind 0 free-list slot) for a small size.
pub fn class_index(size: usize) -> usize {
    CLASS_OF[((size + 15) & !15) / 8] as usize
}

/// Make sure this thread's cache returns its blocks at thread exit (JIT code pushes
/// to the free lists directly).
#[inline]
pub fn ensure_registered() {
    unsafe {
        let tc = &mut *(&raw mut TC);
        if !tc.registered {
            register(tc);
        }
    }
}

/// One bit per SEGMENT-aligned 4 MiB granule of the 47-bit address space (4 MiB of
/// lazily-zeroed BSS): set iff the granule is a tlab segment.
static SEGMENT_MAP: [std::sync::atomic::AtomicU64; (1usize << 47) / SEGMENT / 64] =
    [const { std::sync::atomic::AtomicU64::new(0) }; (1usize << 47) / SEGMENT / 64];

/// Does `p` point into memory managed by this allocator? (Used by PyObject_Free,
/// which receives both tlab blocks and system-allocator blocks.)
#[inline]
pub fn owns(p: *const u8) -> bool {
    let g = p as usize / SEGMENT;
    if g >= (1usize << 47) / SEGMENT {
        return false;
    }
    SEGMENT_MAP[g / 64].load(Acquire) & (1 << (g % 64)) != 0
}

/// The allocator depot, held across fork() so that the child never inherits
/// it locked by a thread that does not exist there (see crate::fork).
pub struct DepotForkGuard(#[allow(dead_code)] std::sync::MutexGuard<'static, Option<Depot>>);

pub fn lock_depot_for_fork() -> DepotForkGuard {
    DepotForkGuard(DEPOT.lock().unwrap_or_else(|e| e.into_inner()))
}
