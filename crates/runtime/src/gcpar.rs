//! (scale3) Parallel, owner-affine analysis for young collections (HotSpot
//! ParallelGC-style work sharing, done by the stopped mutator threads themselves).
//!
//! While the world is stopped for a young collection, the threads parked at a
//! safepoint have nothing to do; their caches also hold the objects they allocated
//! since the last collection. The collector therefore publishes the analysis as a job
//! of small tasks: each task is a chunk of one thread's young buffer (young.rs) or of
//! the orphaned entries, plus one root task per thread (its frames) and one for the
//! interpreter's `sys` roots. Parked threads join the job (`help`, called from
//! `gc::wait_for_release`); every worker takes the tasks of its own buffer and of its
//! own frames first, then any others, so a thread mostly touches objects it created.
//! Threads that are blocked in native calls simply do not join: their tasks are done
//! by the others (the collector always works too).
//!
//! The analysis is gc.rs's trial deletion on the owner words (`GRAPH_TAG` encoding),
//! with atomic updates and phases separated by task counters:
//! 1. snapshot: claim the buffered objects (they become old), drop tuples of atomic
//!    items, store `refcount << 3 | GRAPH_TAG` in each owner word (old one saved);
//! 2. mark everything reachable from the roots and, concurrently, subtract the
//!    references among examined objects that are not marked yet (subtracting from an
//!    object that is marked later is harmless: trial deletion may subtract every
//!    internal reference; skipping marked ones only saves work);
//! 3. objects still referenced from outside become reachable roots; marking runs to
//!    global termination (work packets shared through a pool);
//! 4. unmarked objects are unreachable; owner words are restored.
//! The result is the same set the serial `find_unreachable` computes.

use crate::ffi::*;
use crate::gc::{GRAPH_TAG, REACHABLE, REFS_SHIFT};
use crate::threads::ThreadState;
use std::cell::UnsafeCell;
use std::ffi::{c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::*};

/// Buffer entries per task.
const CHUNK: usize = 2048;
/// Mark-stack entries moved to the shared pool at a time.
const PACKET: usize = 256;
/// Below this many buffered entries the serial analysis is used.
pub const MIN_PARALLEL: usize = 4096;

const S_SNAP: usize = 0;
const S_DECREF: usize = 1;
const S_SCAN: usize = 2;
const S_RESTORE: usize = 3;

const K_ENTRIES: u8 = 0;
const K_LIST: u8 = 1;
const K_PAGES: u8 = 2;
/// Pages per task of a full collection's heap walk.
const PAGES_PER_TASK: usize = 8;

struct Task {
    /// Thread state whose buffer the entries come from (0: none).
    owner: usize,
    src: *const usize,
    len: usize,
    /// K_ENTRIES: `src[..len]` are young-buffer entries; K_LIST: objects already
    /// chosen (large objects); K_PAGES: segment `src as usize`, pages from `len`.
    kind: u8,
    claimed: [AtomicBool; 4],
    objs: UnsafeCell<Vec<*mut PyObject>>,
    saved: UnsafeCell<Vec<usize>>,
    /// Unreachable objects found in this task, each with a new reference.
    unreachable: UnsafeCell<Vec<*mut PyObject>>,
    untracked: UnsafeCell<usize>,
    /// What the unreachable objects need beyond tp_clear (G_* bits), and those that
    /// need type-id / sharded-count handling or weakref detaching.
    flags: UnsafeCell<u32>,
    special: UnsafeCell<Vec<*mut PyObject>>,
    weak: UnsafeCell<Vec<*mut PyObject>>,
    /// Unreachable objects that are the managed __dict__ of another unreachable one.
    dicts: UnsafeCell<usize>,
}

struct RootTask {
    /// Thread state whose frames are roots (0: the interpreter's `sys` roots).
    ts: usize,
    claimed: AtomicBool,
}

struct Pool {
    packets: Vec<Vec<*mut PyObject>>,
    idle: usize,
    workers: usize,
}

struct Job {
    tasks: Vec<Task>,
    roots: Vec<RootTask>,
    remaining: [AtomicUsize; 4],
    pool: Mutex<Pool>,
    terminated: AtomicBool,
}

struct JobCell(UnsafeCell<Option<Job>>);
unsafe impl Sync for JobCell {}

/// The job slot. Written by the collector only while no helper is inside (EPOCH even
/// and REFS zero); read by helpers between joining and leaving.
static JOB: JobCell = JobCell(UnsafeCell::new(None));
/// Odd while a job is open.
static EPOCH: AtomicU64 = AtomicU64::new(0);
/// Helpers inside the job.
static REFS: AtomicUsize = AtomicUsize::new(0);
/// Jobs run / helper participations (diagnostics).
pub static JOBS: AtomicUsize = AtomicUsize::new(0);
pub static HELPS: AtomicUsize = AtomicUsize::new(0);
pub static NO_HELPERS: AtomicUsize = AtomicUsize::new(0);
pub static TOO_SMALL: AtomicUsize = AtomicUsize::new(0);

#[thread_local]
static mut LAST_HELPED: u64 = 0;

/// Is a job open (cheap check for parked threads)?
#[inline]
pub fn open() -> bool {
    EPOCH.load(Acquire) & 1 == 1
}

/// Called by a thread parked at a safepoint: join the open job, if any, once.
#[cold]
pub fn help() {
    let e = EPOCH.load(SeqCst);
    if e & 1 == 0 || unsafe { LAST_HELPED } == e {
        return;
    }
    REFS.fetch_add(1, SeqCst);
    if EPOCH.load(SeqCst) == e {
        unsafe {
            LAST_HELPED = e;
            if let Some(job) = (*JOB.0.get()).as_ref() {
                HELPS.fetch_add(1, Relaxed);
                let mut clock = crate::scalestats::PhaseClock::start();
                run_worker(job, crate::threads::current_ptr() as usize);
                clock.lap(7);
            }
        }
    }
    REFS.fetch_sub(1, SeqCst);
}

struct Worker<'a> {
    job: &'a Job,
    stack: Vec<*mut PyObject>,
}

impl Worker<'_> {
    #[inline(always)]
    fn push(&mut self, op: *mut PyObject) {
        self.stack.push(op);
        if self.stack.len() >= 2 * PACKET {
            self.donate();
        }
    }
    #[cold]
    fn donate(&mut self) {
        let at = self.stack.len() - PACKET;
        let p = self.stack.split_off(at);
        self.job.pool.lock().unwrap_or_else(|e| e.into_inner()).packets.push(p);
    }
    /// Mark everything reachable from the local stack (and from pool packets).
    fn drain(&mut self) {
        loop {
            while let Some(op) = self.stack.pop() {
                unsafe { trace(op, self) };
            }
            let p = self.job.pool.lock().unwrap_or_else(|e| e.into_inner()).packets.pop();
            match p {
                Some(p) => self.stack = p,
                None => return,
            }
        }
    }
    /// Drain until marking has terminated globally (every worker idle, no packets).
    /// Only called once every task that can create marking work is done.
    fn drain_to_termination(&mut self) {
        loop {
            self.drain();
            let mut g = self.job.pool.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(p) = g.packets.pop() {
                self.stack = p;
                continue;
            }
            g.idle += 1;
            let mut spins = 0u32;
            loop {
                if self.job.terminated.load(Acquire) {
                    return;
                }
                if g.idle == g.workers {
                    self.job.terminated.store(true, Release);
                    return;
                }
                if let Some(p) = g.packets.pop() {
                    g.idle -= 1;
                    self.stack = p;
                    break;
                }
                drop(g);
                backoff(&mut spins);
                g = self.job.pool.lock().unwrap_or_else(|e| e.into_inner());
            }
        }
    }
}

#[inline]
fn backoff(spins: &mut u32) {
    if *spins < 32 {
        std::hint::spin_loop();
    } else {
        std::thread::yield_now();
    }
    *spins += 1;
}

/// Mark `op` reachable if it is examined and not marked yet; push it then.
#[inline(always)]
unsafe fn mark(op: *mut PyObject, w: &mut Worker) {
    unsafe {
        let t = &(*op).ob_tid;
        let v = t.load(Relaxed);
        if v & (GRAPH_TAG | REACHABLE) == GRAPH_TAG && t.fetch_or(REACHABLE, Relaxed) & REACHABLE == 0 {
            w.push(op);
        }
    }
}

#[inline(always)]
unsafe fn decref(op: *mut PyObject) {
    unsafe {
        let t = &(*op).ob_tid;
        if t.load(Relaxed) & GRAPH_TAG == GRAPH_TAG {
            t.fetch_sub(1 << REFS_SHIFT, Relaxed);
        }
    }
}

unsafe extern "C" fn visit_mark(op: *mut PyObject, arg: *mut c_void) -> c_int {
    if !op.is_null() {
        unsafe { mark(op, &mut *(arg as *mut Worker)) };
    }
    0
}

unsafe extern "C" fn visit_decref(op: *mut PyObject, _arg: *mut c_void) -> c_int {
    if !op.is_null() {
        unsafe { decref(op) };
    }
    0
}

unsafe fn trace(op: *mut PyObject, w: &mut Worker) {
    unsafe {
        let wp = w as *mut Worker as *mut c_void;
        crate::gc::traverse_inline(op, Some(visit_mark), wp, |it| mark(it, &mut *(wp as *mut Worker)));
    }
}

/// Take a task of `phase`, the caller's own first.
fn claim_task(job: &Job, phase: usize, me: usize) -> Option<usize> {
    for (i, t) in job.tasks.iter().enumerate() {
        if t.owner == me && !t.claimed[phase].load(Relaxed) && !t.claimed[phase].swap(true, AcqRel) {
            return Some(i);
        }
    }
    for (i, t) in job.tasks.iter().enumerate() {
        if !t.claimed[phase].load(Relaxed) && !t.claimed[phase].swap(true, AcqRel) {
            return Some(i);
        }
    }
    None
}

fn claim_root(job: &Job, me: usize) -> Option<usize> {
    for (i, r) in job.roots.iter().enumerate() {
        if r.ts == me && !r.claimed.load(Relaxed) && !r.claimed.swap(true, AcqRel) {
            return Some(i);
        }
    }
    for (i, r) in job.roots.iter().enumerate() {
        if !r.claimed.load(Relaxed) && !r.claimed.swap(true, AcqRel) {
            return Some(i);
        }
    }
    None
}

/// Wait until every task of `phase` is finished (draining marking work meanwhile if
/// `w` is given).
fn wait_phase(job: &Job, phase: usize, mut w: Option<&mut Worker>) {
    let mut spins = 0u32;
    while job.remaining[phase].load(Acquire) != 0 {
        if let Some(w) = w.as_deref_mut() {
            w.drain();
        }
        backoff(&mut spins);
    }
}

unsafe fn snap_task(t: &Task) {
    unsafe {
        let objs = &mut *t.objs.get();
        if t.kind == K_ENTRIES {
            objs.reserve(t.len);
        }
        if t.kind == K_LIST {
            let entries = std::slice::from_raw_parts(t.src, t.len);
            objs.extend(entries.iter().map(|&p| p as *mut PyObject));
        } else {
            let tt = crate::types::tuple::tuple_type();
            let mut untracked = 0usize;
            let mut take = |op: *mut PyObject| {
                if (*op).type_ptr() == tt && atomic_tuple(op) {
                    // a tuple of untracked items can never be part of a cycle
                    let b = &(*op).ob_gc_bits;
                    b.fetch_and(!(crate::alloc::GC_BIT_TRACKED | crate::alloc::GC_BIT_OLD | crate::young::GC_BIT_YOUNG_BUF), Relaxed);
                    crate::gc::note_untracked();
                    untracked += 1;
                    return;
                }
                objs.push(op);
            };
            if t.kind == K_ENTRIES {
                for &p in std::slice::from_raw_parts(t.src, t.len) {
                    if crate::young::claim(p, true, true) {
                        take(p as *mut PyObject);
                    }
                }
            } else {
                // every tracked object of these pages: it becomes old and leaves its
                // young buffer (each block is visited by exactly one task)
                crate::tlab::for_each_gc_block_in(t.src as usize, t.len, t.len + PAGES_PER_TASK, |op| {
                    let b = &(*op).ob_gc_bits;
                    let bits = b.load(Relaxed);
                    if bits & crate::alloc::GC_BIT_TRACKED != 0 {
                        b.store((bits | crate::alloc::GC_BIT_OLD) & !crate::young::GC_BIT_YOUNG_BUF, Relaxed);
                        take(op);
                    }
                });
            }
            *t.untracked.get() = untracked;
        }
        // trial-deletion state in the owner words (gc.rs `find_unreachable`)
        let saved = &mut *t.saved.get();
        saved.reserve(objs.len());
        for &o in objs.iter() {
            let r = crate::gc::total_refcnt(&*o);
            let w = &(*o).ob_tid;
            saved.push(w.load(Relaxed));
            w.store(((r.clamp(0, 1 << 56) as usize) << REFS_SHIFT) | GRAPH_TAG, Relaxed);
        }
    }
}

unsafe fn atomic_tuple(op: *mut PyObject) -> bool {
    unsafe {
        let len = crate::object::var_size(&*op) as usize;
        let items = (*(op as *mut PyTupleObject)).ob_item.as_ptr();
        for i in 0..len {
            let it = *items.add(i);
            if it.is_null() || crate::gc::is_tracked(&*it) {
                return false;
            }
        }
        true
    }
}

unsafe fn decref_task(t: &Task) {
    unsafe {
        for &op in (*t.objs.get()).iter() {
            if (*op).ob_tid.load(Relaxed) & REACHABLE == 0 {
                crate::gc::traverse_inline(op, Some(visit_decref), std::ptr::null_mut(), |it| decref(it));
            }
        }
    }
}

unsafe fn scan_task(t: &Task, w: &mut Worker) {
    unsafe {
        for &op in (*t.objs.get()).iter() {
            let tw = &(*op).ob_tid;
            let v = tw.load(Relaxed);
            if (v as isize) >> REFS_SHIFT > 0 && v & REACHABLE == 0 && tw.fetch_or(REACHABLE, Relaxed) & REACHABLE == 0 {
                w.push(op);
            }
        }
    }
}

/// The unreachable objects have legacy finalizers (tp_del).
pub const G_LEGACY: u32 = 1;
/// Some unreachable object has a finalizer that has not run yet.
pub const G_FINALIZE: u32 = 2;
/// A weakref to some unreachable object has a callback.
pub const G_WEAKCB: u32 = 4;

unsafe fn restore_task(t: &Task) {
    unsafe {
        let un = &mut *t.unreachable.get();
        let objs = &*t.objs.get();
        let saved = &*t.saved.get();
        let mut dicts = 0usize;
        for (&o, &tid) in objs.iter().zip(saved.iter()) {
            let w = &(*o).ob_tid;
            if w.load(Relaxed) & REACHABLE == 0 {
                un.push(o);
                // (for the CPython-compatible count: a managed __dict__ is part of its
                // instance there) the dict's state word, if it is examined and not yet
                // restored by its own task
                if crate::alloc::has_managed(o) {
                    let d = *crate::alloc::managed_dict_slot(o) as usize;
                    if d != 0 && d & 1 == 0 {
                        let dv = (*(d as *mut PyObject)).ob_tid.load(Relaxed);
                        if dv & GRAPH_TAG == GRAPH_TAG && dv & REACHABLE == 0 {
                            dicts += 1;
                        }
                    }
                }
            }
            w.store(tid, Relaxed);
        }
        *t.dicts.get() = dicts;
        // what the garbage needs (owner words are real again: references may be taken)
        let mut flags = 0u32;
        let special = &mut *t.special.get();
        let weak = &mut *t.weak.get();
        for &o in un.iter() {
            let tp = (*o).type_ptr();
            if (*tp).tp_del.is_some() {
                flags |= G_LEGACY;
            }
            if (*tp).tp_finalize.is_some() && !crate::gc::is_finalized(o) {
                flags |= G_FINALIZE;
            }
            if crate::object::has_type_id(o) || crate::sharded::is_sharded(&*o) {
                // counted through per-thread tables: the collector takes the reference
                // itself (a delta left in this thread's table would outlive the
                // unsharding of the object, see sharded.rs)
                special.push(o);
            } else {
                crate::object::incref(o);
            }
            match crate::types::weakref::gc_weak_state(o) {
                0 => {}
                1 => weak.push(o),
                _ => flags |= G_WEAKCB,
            }
        }
        *t.flags.get() = flags;
    }
}

unsafe fn run_worker(job: &Job, me: usize) {
    unsafe {
        let mut w = Worker { job, stack: Vec::new() };
        {
            let mut g = job.pool.lock().unwrap_or_else(|e| e.into_inner());
            if job.terminated.load(Acquire) {
                drop(g);
            } else {
                g.workers += 1;
            }
        }
        // 1. snapshot
        while let Some(i) = claim_task(job, S_SNAP, me) {
            snap_task(&job.tasks[i]);
            job.remaining[S_SNAP].fetch_sub(1, AcqRel);
        }
        wait_phase(job, S_SNAP, None);
        // 2. roots, then subtract internal references
        while let Some(i) = claim_root(job, me) {
            let ts = job.roots[i].ts;
            if ts == 0 {
                for p in crate::gc::sys_roots() {
                    mark(p, &mut w);
                }
            } else {
                crate::gc::thread_roots(&*(ts as *const ThreadState), |p| mark(p, &mut w));
            }
            w.drain();
            job.remaining[S_DECREF].fetch_sub(1, AcqRel);
        }
        while let Some(i) = claim_task(job, S_DECREF, me) {
            decref_task(&job.tasks[i]);
            job.remaining[S_DECREF].fetch_sub(1, AcqRel);
            w.drain();
        }
        wait_phase(job, S_DECREF, Some(&mut w));
        // 3. externally referenced objects are roots too; mark to termination
        while let Some(i) = claim_task(job, S_SCAN, me) {
            scan_task(&job.tasks[i], &mut w);
            w.drain();
            job.remaining[S_SCAN].fetch_sub(1, AcqRel);
        }
        wait_phase(job, S_SCAN, Some(&mut w));
        if !job.terminated.load(Acquire) {
            w.drain_to_termination();
        }
        // 4. collect the unreachable, restore the owner words
        while let Some(i) = claim_task(job, S_RESTORE, me) {
            restore_task(&job.tasks[i]);
            job.remaining[S_RESTORE].fetch_sub(1, AcqRel);
        }
    }
}

/// Result of a parallel young analysis.
pub struct Analysis {
    pub examined: usize,
    /// G_* bits of all parts.
    pub flags: u32,
    pub parts: Vec<Part>,
    /// Unreachable managed __dict__s of unreachable instances.
    pub dicts: usize,
}

/// The unreachable objects found in one task.
pub struct Part {
    /// Thread state whose young buffer they came from (0: none).
    pub owner: usize,
    /// Each holds a new reference, except those also in `special`.
    pub garbage: Vec<*mut PyObject>,
    /// Need type-id release / unsharding (no reference taken yet).
    pub special: Vec<*mut PyObject>,
    /// Take part in weak referencing (no callbacks).
    pub weak: Vec<*mut PyObject>,
}

impl Analysis {
    pub fn unreachable_count(&self) -> usize {
        self.parts.iter().map(|p| p.garbage.len()).sum()
    }
}

fn new_task(owner: usize, src: *const usize, len: usize, kind: u8) -> Task {
    Task {
        owner,
        src,
        len,
        kind,
        claimed: [const { AtomicBool::new(false) }; 4],
        objs: UnsafeCell::new(Vec::new()),
        saved: UnsafeCell::new(Vec::new()),
        unreachable: UnsafeCell::new(Vec::new()),
        untracked: UnsafeCell::new(0),
        flags: UnsafeCell::new(0),
        special: UnsafeCell::new(Vec::new()),
        weak: UnsafeCell::new(Vec::new()),
        dicts: UnsafeCell::new(0),
    }
}

/// `RPY_GC_PARALLEL=0`: always analyse serially (diagnostics, measurements).
fn parallel_enabled() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var_os("RPY_GC_PARALLEL").is_none_or(|v| v != "0"))
}

/// Is a parallel analysis worth starting (somebody to help)?
fn helpers_available(me: *mut ThreadState) -> bool {
    if !parallel_enabled() {
        return false;
    }
    if crate::gc::helper_candidates() != 0 {
        return true;
    }
    NO_HELPERS.fetch_add(1, Relaxed);
    if crate::scalestats::gc_debug() {
        let v: Vec<(u8, bool)> = crate::threads::ALL_THREADS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|&t| unsafe {
                let t = &*(t as *const ThreadState);
                (t.mode.0.load(SeqCst), t.alive())
            })
            .collect();
        eprintln!("nohelp me={:?} {:?}", me, v);
    }
    false
}

/// World stopped, called by the collector for a young collection: run the analysis
/// in parallel with the parked threads. None (nothing done, buffers untouched) when it
/// is not worth it: few young objects or nobody parked to help.
pub unsafe fn analyse_young(me: *mut ThreadState) -> Option<Analysis> {
    unsafe {
        if !helpers_available(me) {
            return None;
        }
        let mut bs = crate::young::take_buffers();
        let total: usize = bs.orphans.len() + bs.bufs().iter().map(|&b| (*(b as *mut crate::young::YoungBuf)).entries.len()).sum::<usize>();
        if total < MIN_PARALLEL {
            TOO_SMALL.fetch_add(1, Relaxed);
            crate::young::give_back_orphans(std::mem::take(&mut bs.orphans));
            return None;
        }
        let mut tasks = Vec::new();
        for &bp in bs.bufs() {
            let b = &*(bp as *mut crate::young::YoungBuf);
            for c in b.entries.chunks(CHUNK) {
                tasks.push(new_task(b.ts, c.as_ptr(), c.len(), K_ENTRIES));
            }
        }
        for c in bs.orphans.chunks(CHUNK) {
            tasks.push(new_task(0, c.as_ptr(), c.len(), K_ENTRIES));
        }
        let mut large: Vec<usize> = Vec::new();
        crate::alloc::for_each_large_gc(|op| {
            if (*op).ob_gc_bits.load(Relaxed) & (crate::alloc::GC_BIT_TRACKED | crate::alloc::GC_BIT_OLD) == crate::alloc::GC_BIT_TRACKED {
                // young large objects become old like the buffered ones
                (*op).ob_gc_bits.fetch_or(crate::alloc::GC_BIT_OLD, Relaxed);
                large.push(op as usize);
            }
        });
        if !large.is_empty() {
            tasks.push(new_task(0, large.as_ptr(), large.len(), K_LIST));
        }
        let a = execute(me, tasks);
        for &bp in bs.bufs() {
            (*(bp as *mut crate::young::YoungBuf)).entries.clear();
        }
        drop(bs);
        Some(a)
    }
}

/// World stopped, called by the collector for a full collection: the same analysis
/// over every tracked object, found by walking the GC pages in parallel (tasks of
/// PAGES_PER_TASK pages, no affinity) plus the large-object registry. Every examined
/// object becomes old and leaves the young buffers. None when not worth it.
pub unsafe fn analyse_full(me: *mut ThreadState) -> Option<Analysis> {
    unsafe {
        if crate::gc::tracked_estimate() < MIN_PARALLEL || !helpers_available(me) {
            return None;
        }
        let mut tasks = Vec::new();
        for seg in 0..crate::tlab::segment_count() {
            let mut p = 0;
            while p < crate::tlab::PAGES_PER_SEGMENT {
                // src encodes the segment, len the first page (see snap_task)
                tasks.push(new_task(0, seg as *const usize, p, K_PAGES));
                p += PAGES_PER_TASK;
            }
        }
        let mut large: Vec<usize> = Vec::new();
        crate::alloc::for_each_large_gc(|op| {
            let b = &(*op).ob_gc_bits;
            if b.load(Relaxed) & crate::alloc::GC_BIT_TRACKED != 0 {
                b.fetch_or(crate::alloc::GC_BIT_OLD, Relaxed);
                large.push(op as usize);
            }
        });
        if !large.is_empty() {
            tasks.push(new_task(0, large.as_ptr(), large.len(), K_LIST));
        }
        let a = execute(me, tasks);
        // every buffered object was examined, its buffer bit cleared by the walk
        let mut bs = crate::young::take_buffers();
        bs.orphans.clear();
        for &bp in bs.bufs() {
            (*(bp as *mut crate::young::YoungBuf)).entries.clear();
        }
        drop(bs);
        Some(a)
    }
}

/// Run a job made of `tasks` (plus the root tasks) with the parked threads and
/// collect its results.
unsafe fn execute(me: *mut ThreadState, tasks: Vec<Task>) -> Analysis {
    unsafe {
        let mut roots = vec![RootTask { ts: 0, claimed: AtomicBool::new(false) }];
        for &t in crate::threads::ALL_THREADS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            roots.push(RootTask { ts: t, claimed: AtomicBool::new(false) });
        }
        let n = tasks.len();
        let job = Job {
            remaining: [AtomicUsize::new(n), AtomicUsize::new(n + roots.len()), AtomicUsize::new(n), AtomicUsize::new(n)],
            tasks,
            roots,
            pool: Mutex::new(Pool { packets: Vec::new(), idle: 0, workers: 0 }),
            terminated: AtomicBool::new(false),
        };
        *JOB.0.get() = Some(job);
        let job = (*JOB.0.get()).as_ref().unwrap();
        JOBS.fetch_add(1, Relaxed);
        EPOCH.fetch_add(1, SeqCst); // open
        crate::gc::wake_parked();
        run_worker(job, me as usize);
        let mut spins = 0u32;
        while job.remaining[S_RESTORE].load(Acquire) != 0 {
            backoff(&mut spins);
        }
        EPOCH.fetch_add(1, SeqCst); // closed
        while REFS.load(SeqCst) != 0 {
            backoff(&mut spins);
        }
        let mut examined = 0usize;
        let mut untracked = 0usize;
        let mut flags = 0u32;
        let mut dicts = 0usize;
        let mut parts = Vec::new();
        for t in &job.tasks {
            examined += (*t.objs.get()).len();
            untracked += *t.untracked.get();
            flags |= *t.flags.get();
            dicts += *t.dicts.get();
            let garbage = std::mem::take(&mut *t.unreachable.get());
            if !garbage.is_empty() {
                parts.push(Part { owner: t.owner, garbage, special: std::mem::take(&mut *t.special.get()), weak: std::mem::take(&mut *t.weak.get()) });
            }
        }
        crate::gc_support::UNTRACKED_TUPLES.fetch_add(untracked, Relaxed);
        *JOB.0.get() = None;
        Analysis { examined, flags, parts, dicts }
    }
}
