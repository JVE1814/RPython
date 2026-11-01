//! Small-object allocator (HotSpot TLAB / tcmalloc style).
//!
//! * Memory comes from 4 MiB segments (mmap), split into 64 KiB pages.
//! * Each page serves one size class; the page header (first 64 bytes) records it,
//!   so `free` needs only the pointer (page = ptr & !(PAGE-1)).
//! * Each thread keeps a free list per size class (no atomics on the fast path).
//!   Frees from any thread go to the freeing thread's cache (blocks are
//!   interchangeable). Overfull caches spill batches to a global depot; empty caches
//!   refill from it, and only then carve fresh pages.
//! * Blocks larger than the biggest class are served by the system allocator.

use std::sync::Mutex;

pub const PAGE: usize = 64 * 1024;
const SEGMENT: usize = 4 * 1024 * 1024;
const HEADER: usize = 64;
const MAGIC: u32 = 0x5250_5947; // "RPYG"

pub const CLASSES: [usize; 24] = [
    16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256, 288, 320, 384, 448, 512, 640, 768, 896, 1024, 2048,
];
pub const MAX_SMALL: usize = 2048;
const NCLASSES: usize = CLASSES.len();

/// size -> class index lookup for sizes up to 2048 in 16-byte steps.
static CLASS_OF: [u8; MAX_SMALL / 16 + 1] = {
    let mut t = [0u8; MAX_SMALL / 16 + 1];
    let mut i = 0;
    while i <= MAX_SMALL / 16 {
        let size = i * 16;
        let mut c = 0;
        while CLASSES[c] < size {
            c += 1;
        }
        t[i] = c as u8;
        i += 1;
    }
    t
};

#[repr(C)]
struct PageHeader {
    magic: u32,
    class: u32,
    block: usize,
}

#[inline(always)]
pub fn class_for(size: usize) -> Option<usize> {
    if size > MAX_SMALL {
        return None;
    }
    Some(CLASS_OF[(size + 15) / 16] as usize)
}

struct Depot {
    /// batches of free blocks per class
    batches: Vec<Vec<Vec<usize>>>,
    /// current segment carve state
    seg_cur: usize,
    seg_end: usize,
    segments: Vec<usize>,
}

fn new_page(d: &mut Depot, class: usize) -> Vec<usize> {
    if d.seg_cur + PAGE > d.seg_end {
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
                return Vec::new();
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
            SEGMENT_MAP[g / 64].fetch_or(1 << (g % 64), std::sync::atomic::Ordering::Release);
            d.seg_cur = start;
            d.seg_end = start + SEGMENT;
            d.segments.push(start);
        }
    }
    let page = d.seg_cur;
    d.seg_cur += PAGE;
    let block = CLASSES[class];
    unsafe {
        let h = page as *mut PageHeader;
        (*h).magic = MAGIC;
        (*h).class = class as u32;
        (*h).block = block;
    }
    let first = page + HEADER.max(block.min(HEADER));
    let first = (first + 15) & !15;
    let n = (page + PAGE - first) / block;
    let mut v = Vec::with_capacity(n);
    for i in (0..n).rev() {
        v.push(first + i * block);
    }
    v
}

static DEPOT: Mutex<Option<Depot>> = Mutex::new(None);

const BATCH: usize = 64;
const CACHE_LIMIT: usize = 256;

fn new_depot() -> Depot {
    Depot { batches: (0..NCLASSES).map(|_| Vec::new()).collect(), seg_cur: 0, seg_end: 0, segments: Vec::new() }
}

/// Per-thread cache: one intrusive free list per size class (the first word of a
/// free block links to the next one). `#[thread_local]` gives direct fs-relative
/// access without lazy-init checks; a separate guard returns the blocks to the depot
/// when the thread exits.
struct ThreadCache {
    heads: [usize; NCLASSES],
    counts: [u32; NCLASSES],
    registered: bool,
    dead: bool,
}

#[thread_local]
static mut TC: ThreadCache = ThreadCache { heads: [0; NCLASSES], counts: [0; NCLASSES], registered: false, dead: false };

struct ExitGuard;
impl Drop for ExitGuard {
    fn drop(&mut self) {
        unsafe {
            let tc = &mut *(&raw mut TC);
            tc.dead = true;
            let mut g = DEPOT.lock().unwrap_or_else(|e| e.into_inner());
            let d = g.get_or_insert_with(new_depot);
            for c in 0..NCLASSES {
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
                }
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
    let _ = GUARD.try_with(|_| ());
}

#[cold]
fn refill(class: usize, tc: &mut ThreadCache) {
    if !tc.registered {
        register(tc);
    }
    let blocks = {
        let mut g = DEPOT.lock().unwrap_or_else(|e| e.into_inner());
        let d = g.get_or_insert_with(new_depot);
        match d.batches[class].pop() {
            Some(b) => b,
            None => new_page(d, class),
        }
    };
    for p in blocks {
        unsafe { *(p as *mut usize) = tc.heads[class] };
        tc.heads[class] = p;
        tc.counts[class] += 1;
    }
}

#[cold]
fn spill(class: usize, tc: &mut ThreadCache) {
    let mut batch = Vec::with_capacity(BATCH);
    for _ in 0..BATCH {
        let p = tc.heads[class];
        if p == 0 {
            break;
        }
        tc.heads[class] = unsafe { *(p as *const usize) };
        tc.counts[class] -= 1;
        batch.push(p);
    }
    let mut g = DEPOT.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(new_depot).batches[class].push(batch);
}

#[inline(always)]
unsafe fn zero_block(p: *mut u8, size: usize) {
    unsafe {
        // constant-size stores for the common small classes (avoids a memset call)
        match size {
            16 => std::ptr::write_bytes(p, 0, 16),
            32 => std::ptr::write_bytes(p, 0, 32),
            48 => std::ptr::write_bytes(p, 0, 48),
            64 => std::ptr::write_bytes(p, 0, 64),
            80 => std::ptr::write_bytes(p, 0, 80),
            96 => std::ptr::write_bytes(p, 0, 96),
            _ => std::ptr::write_bytes(p, 0, size),
        }
    }
}

/// Allocate a zeroed small block (size <= MAX_SMALL). Null on OOM.
#[inline]
pub fn alloc_small(size: usize) -> *mut u8 {
    let c = CLASS_OF[(size + 15) / 16] as usize;
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
        zero_block(p as *mut u8, CLASSES[c]);
        p as *mut u8
    }
}

#[cold]
fn alloc_from_depot(c: usize) -> *mut u8 {
    // thread is exiting: take directly from the depot
    let mut g = DEPOT.lock().unwrap_or_else(|e| e.into_inner());
    let d = g.get_or_insert_with(new_depot);
    let mut v = match d.batches[c].pop() {
        Some(b) => b,
        None => new_page(d, c),
    };
    let p = v.pop().unwrap_or(0);
    if !v.is_empty() {
        d.batches[c].push(v);
    }
    if p != 0 {
        unsafe { std::ptr::write_bytes(p as *mut u8, 0, CLASSES[c]) };
    }
    p as *mut u8
}

#[inline]
pub fn small_class_of(p: *mut u8) -> usize {
    let page = (p as usize) & !(PAGE - 1);
    unsafe {
        let h = page as *const PageHeader;
        debug_assert_eq!((*h).magic, MAGIC);
        (*h).class as usize
    }
}

#[inline]
pub fn small_size_of(p: *mut u8) -> usize {
    CLASSES[small_class_of(p)]
}

/// Free a block returned by `alloc_small`.
#[inline]
pub fn free_small(p: *mut u8) {
    let c = small_class_of(p);
    unsafe {
        let tc = &mut *(&raw mut TC);
        if tc.dead {
            let mut g = DEPOT.lock().unwrap_or_else(|e| e.into_inner());
            g.get_or_insert_with(new_depot).batches[c].push(vec![p as usize]);
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

/// Size class index for a small size.
pub fn class_index(size: usize) -> usize {
    CLASS_OF[(size + 15) / 16] as usize
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
    SEGMENT_MAP[g / 64].load(std::sync::atomic::Ordering::Acquire) & (1 << (g % 64)) != 0
}

/// The allocator depot, held across fork() so that the child never inherits
/// it locked by a thread that does not exist there (see crate::fork).
pub struct DepotForkGuard(#[allow(dead_code)] std::sync::MutexGuard<'static, Option<Depot>>);

pub fn lock_depot_for_fork() -> DepotForkGuard {
    DepotForkGuard(DEPOT.lock().unwrap_or_else(|e| e.into_inner()))
}
