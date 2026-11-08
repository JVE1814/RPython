//! (scale3) Per-thread young-generation buffers.
//!
//! Every object that becomes tracked and young is appended to a buffer owned by the
//! OS thread that tracked it (HotSpot: the young generation is allocated per thread).
//! Young collections take their candidates from these buffers instead of walking every
//! GC page, so their cost follows the number of young objects rather than the heap
//! size, and each thread's young objects form a partition with good cache affinity for
//! the parallel collector (gcpar.rs): the thread that created the objects analyses and
//! clears them.
//!
//! An object is in some buffer while it carries `GC_BIT_YOUNG_BUF`. Buffers are not
//! told about frees: entries go stale when an object dies (the free path zeroes the GC
//! bits) and may then point at a block that was reused for another object. Blocks of
//! the small-object allocator stay mapped for the life of the process, so reading a
//! stale entry is harmless; large objects (system allocator) are never buffered and the
//! collector finds young large objects through the allocator's registry. A reader keeps
//! an entry only if the object still has the bit, is tracked and is not old; it then
//! clears the bit (claim), which also drops later duplicates of the same address.
//!
//! Buffers are touched by their own thread (append, compaction) while it runs, and by
//! the collector only while the world is stopped (no thread allocates then).

use crate::alloc::{GC_BIT_LARGE, GC_BIT_OLD, GC_BIT_TRACKED};
use crate::ffi::PyObject;
use std::sync::Mutex;
use std::sync::atomic::Ordering::*;

/// The object has an entry in a young buffer.
pub const GC_BIT_YOUNG_BUF: u8 = 0x08;

/// One thread's buffer (a thread-local static; the registry holds its address while
/// the thread lives).
pub struct YoungBuf {
    pub entries: Vec<usize>,
    /// Compact (drop stale entries) when `entries` reaches this length; 0 until the
    /// buffer is registered (and again once the thread exits), so that appends take
    /// the slow path.
    limit: usize,
    /// The thread state of the thread owning this buffer when it last compacted (for
    /// partition affinity; may be stale).
    pub ts: usize,
}

const MIN_LIMIT: usize = 4096;

/// Every live thread's buffer (addresses of the threads' `BUF`).
static REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// Entries of threads that exited (and appends from threads whose buffer is gone).
static ORPHANS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

#[thread_local]
static mut BUF: YoungBuf = YoungBuf { entries: Vec::new(), limit: 0, ts: 0 };
/// 0: not registered yet, 1: registered, 2: thread exiting (append to the orphans).
#[thread_local]
static mut STATE: u8 = 0;

struct ExitGuard;
impl Drop for ExitGuard {
    fn drop(&mut self) {
        unsafe {
            let was = STATE;
            STATE = 2;
            if was != 1 {
                return;
            }
            let b = &raw mut BUF;
            let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
            reg.retain(|&p| p != b as usize);
            (*b).limit = 0;
            let entries = std::mem::take(&mut (*b).entries);
            let mut o = ORPHANS.lock().unwrap_or_else(|e| e.into_inner());
            o.extend_from_slice(&entries);
        }
    }
}

thread_local! {
    static GUARD: ExitGuard = const { ExitGuard };
}

/// Record that `op` (a small GC object that just became tracked and young, without the
/// buffer bit) is young. The caller sets the bit.
#[inline(always)]
pub unsafe fn push(op: *mut PyObject) {
    unsafe {
        let b = &mut *(&raw mut BUF);
        let len = b.entries.len();
        if len < b.limit {
            // capacity is reserved up to the limit
            *b.entries.as_mut_ptr().add(len) = op as usize;
            b.entries.set_len(len + 1);
            return;
        }
        push_slow(op)
    }
}

#[cold]
#[inline(never)]
unsafe fn push_slow(op: *mut PyObject) {
    unsafe {
        let b = &mut *(&raw mut BUF);
        if STATE != 1 {
            if STATE == 0 && GUARD.try_with(|_| ()).is_ok() {
                let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
                b.entries.reserve(MIN_LIMIT);
                b.limit = MIN_LIMIT;
                reg.push(b as *mut YoungBuf as usize);
                STATE = 1;
            } else {
                ORPHANS.lock().unwrap_or_else(|e| e.into_inner()).push(op as usize);
                return;
            }
        }
        b.ts = crate::threads::current_ptr() as usize;
        if b.entries.len() >= b.limit {
            compact(b);
        }
        b.entries.push(op as usize);
    }
}

/// Drop the stale entries of the calling thread's own buffer. Writes no object (other
/// threads may own the objects behind stale entries); duplicates of a reused address are
/// dropped by sorting the addresses when dropping stale entries freed too little.
fn compact(b: &mut YoungBuf) {
    let before = b.entries.len();
    b.entries.retain(|&p| unsafe {
        let bits = (*(p as *mut PyObject)).ob_gc_bits.load(Relaxed);
        bits & (GC_BIT_YOUNG_BUF | GC_BIT_TRACKED | GC_BIT_OLD) == GC_BIT_YOUNG_BUF | GC_BIT_TRACKED
    });
    // (wheels3) a block freed and reused for a new tracked object is appended again
    // while its old entry still matches the new object's bits, so a thread that keeps
    // allocating and freeing (without a collection in between: pandas' test suite) grew
    // its buffer by one entry per allocation, up to an 8 GB reserve. When the stale
    // entries were few, drop duplicate addresses (one entry per address is enough; the
    // order is not significant).
    if b.entries.len() > before / 2 {
        b.entries.sort_unstable();
        b.entries.dedup();
    }
    b.limit = (b.entries.len() * 2).max(MIN_LIMIT);
    let need = b.limit - b.entries.len();
    b.entries.reserve(need);
}

/// Is `bits` the state of an object that belongs in a young buffer when it becomes
/// tracked (small GC object without the bit)?
#[inline(always)]
pub fn wants_push(bits: u8) -> bool {
    bits & (GC_BIT_YOUNG_BUF | GC_BIT_LARGE) == 0
}

/// World stopped: the registered buffers (raw pointers) and the orphaned entries.
/// The registry lock is held by the returned guard until it is dropped.
pub struct Buffers {
    reg: std::sync::MutexGuard<'static, Vec<usize>>,
    pub orphans: Vec<usize>,
}

impl Buffers {
    pub fn bufs(&self) -> &[usize] {
        &self.reg
    }
}

pub fn take_buffers() -> Buffers {
    let reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let orphans = std::mem::take(&mut *ORPHANS.lock().unwrap_or_else(|e| e.into_inner()));
    Buffers { reg, orphans }
}

/// Return orphaned entries taken by `take_buffers` but not processed.
pub fn give_back_orphans(v: Vec<usize>) {
    if !v.is_empty() {
        ORPHANS.lock().unwrap_or_else(|e| e.into_inner()).extend(v);
    }
}

/// Claim `p` (an entry) for a collection: true if it is a live young object whose
/// buffer bit was set; the bit is cleared (and, with `promote`, the object becomes old).
/// `atomic`: other workers may claim concurrently (parallel snapshot).
#[inline(always)]
pub unsafe fn claim(p: usize, promote: bool, atomic: bool) -> bool {
    unsafe {
        let b = &(*(p as *mut PyObject)).ob_gc_bits;
        let bits = b.load(Relaxed);
        if bits & GC_BIT_YOUNG_BUF == 0 {
            return false; // stale: the object died (or a duplicate was claimed)
        }
        if bits & (GC_BIT_TRACKED | GC_BIT_OLD) != GC_BIT_TRACKED {
            // not a young tracked object any more: drop the entry and the bit (so that
            // tracking it again appends it again)
            if atomic {
                b.fetch_and(!GC_BIT_YOUNG_BUF, Relaxed);
            } else {
                b.store(bits & !GC_BIT_YOUNG_BUF, Relaxed);
            }
            return false;
        }
        let set = if promote { GC_BIT_OLD } else { 0 };
        if atomic {
            let prev = b.fetch_update(Relaxed, Relaxed, |x| {
                if x & GC_BIT_YOUNG_BUF == 0 { None } else { Some((x & !GC_BIT_YOUNG_BUF) | set) }
            });
            prev.is_ok()
        } else {
            b.store((bits & !GC_BIT_YOUNG_BUF) | set, Relaxed);
            true
        }
    }
}

/// World stopped, after a full collection examined (and promoted) every tracked
/// object: empty every buffer, clearing the bit of the objects still carrying it.
pub fn clear_all() {
    let mut bs = take_buffers();
    let orphans = std::mem::take(&mut bs.orphans);
    let clear = |p: usize| unsafe {
        let b = &(*(p as *mut PyObject)).ob_gc_bits;
        let bits = b.load(Relaxed);
        if bits & GC_BIT_YOUNG_BUF != 0 {
            b.store(bits & !GC_BIT_YOUNG_BUF, Relaxed);
        }
    };
    for &p in &orphans {
        clear(p);
    }
    for &bp in bs.bufs() {
        let b = unsafe { &mut *(bp as *mut YoungBuf) };
        for &p in &b.entries {
            clear(p);
        }
        b.entries.clear();
    }
}

/// Number of entries in all buffers (diagnostics).
pub fn total_entries() -> usize {
    let bs = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    bs.iter().map(|&b| unsafe { (*(b as *mut YoungBuf)).entries.len() }).sum::<usize>()
        + ORPHANS.lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// The buffer locks, held across fork() (see crate::fork): after fork only the calling
/// thread exists; the other threads' buffers stay registered (their entries are still
/// valid candidates) and are processed by collections like any other.
pub struct ForkGuard(
    #[allow(dead_code)] std::sync::MutexGuard<'static, Vec<usize>>,
    #[allow(dead_code)] std::sync::MutexGuard<'static, Vec<usize>>,
);

pub fn lock_for_fork() -> ForkGuard {
    let r = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let o = ORPHANS.lock().unwrap_or_else(|e| e.into_inner());
    ForkGuard(r, o)
}
