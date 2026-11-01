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

use std::cell::RefCell;
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

static DEPOT: Mutex<Option<Depot>> = Mutex::new(None);

const BATCH: usize = 64;
const CACHE_LIMIT: usize = 256;

struct Cache {
    lists: [Vec<usize>; NCLASSES],
}

impl Drop for Cache {
    fn drop(&mut self) {
        let mut g = DEPOT.lock().unwrap();
        let d = g.get_or_insert_with(new_depot);
        for (c, l) in self.lists.iter_mut().enumerate() {
            if !l.is_empty() {
                d.batches[c].push(std::mem::take(l));
            }
        }
    }
}

fn new_depot() -> Depot {
    Depot { batches: (0..NCLASSES).map(|_| Vec::new()).collect(), seg_cur: 0, seg_end: 0, segments: Vec::new() }
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache { lists: std::array::from_fn(|_| Vec::with_capacity(CACHE_LIMIT)) });
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

#[cold]
fn refill(class: usize, list: &mut Vec<usize>) {
    let mut g = DEPOT.lock().unwrap();
    let d = g.get_or_insert_with(new_depot);
    if let Some(b) = d.batches[class].pop() {
        list.extend(b);
        return;
    }
    let blocks = new_page(d, class);
    list.extend(blocks);
}

#[cold]
fn spill(class: usize, list: &mut Vec<usize>) {
    let keep = list.len() - BATCH;
    let batch: Vec<usize> = list.drain(keep..).collect();
    let mut g = DEPOT.lock().unwrap();
    let d = g.get_or_insert_with(new_depot);
    d.batches[class].push(batch);
}

/// Allocate a zeroed small block (size <= MAX_SMALL). Null on OOM.
#[inline]
pub fn alloc_small(size: usize) -> *mut u8 {
    let c = CLASS_OF[(size + 15) / 16] as usize;
    let p = CACHE
        .try_with(|cache| {
            let mut cache = cache.borrow_mut();
            let list = &mut cache.lists[c];
            if list.is_empty() {
                refill(c, list);
            }
            list.pop().unwrap_or(0)
        })
        .unwrap_or_else(|_| {
            // thread is exiting: take directly from the depot
            let mut v = Vec::new();
            refill(c, &mut v);
            let p = v.pop().unwrap_or(0);
            if !v.is_empty() {
                let mut g = DEPOT.lock().unwrap();
                g.get_or_insert_with(new_depot).batches[c].push(v);
            }
            p
        });
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
    let ok = CACHE
        .try_with(|cache| {
            let mut cache = cache.borrow_mut();
            let list = &mut cache.lists[c];
            list.push(p as usize);
            if list.len() > CACHE_LIMIT {
                spill(c, list);
            }
        })
        .is_ok();
    if !ok {
        let mut g = DEPOT.lock().unwrap();
        g.get_or_insert_with(new_depot).batches[c].push(vec![p as usize]);
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
