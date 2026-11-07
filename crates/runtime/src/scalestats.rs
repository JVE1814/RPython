//! Cross-thread cost counters (scale1). Off unless the environment variable
//! `RPY_SCALESTATS` is set when the runtime starts; then a report is printed to stderr
//! at process exit. The counters sit on paths that are already slow when they fire
//! (a non-owner reference count operation, a stop-the-world, a lock wait), and each
//! costs one relaxed load of `ON` when disabled.
//!
//! Counted:
//! * reference count operations on objects owned by another thread, by type (only
//!   those done by runtime code; JIT-inlined increfs are not seen);
//! * objects handed to an owner's merge queue;
//! * stop-the-world operations: count, time spent waiting for the others to stop,
//!   time spent stopped (parked threads);
//! * collections and their total pause;
//! * QSBR batches closed and items retired.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};

pub static ON: AtomicBool = AtomicBool::new(false);

#[inline(always)]
pub fn on() -> bool {
    ON.load(Relaxed)
}

const SLOTS: usize = 1024;
struct Slot {
    tp: AtomicUsize,
    inc: AtomicU64,
    dec: AtomicU64,
}
static TYPES: [Slot; SLOTS] =
    [const { Slot { tp: AtomicUsize::new(0), inc: AtomicU64::new(0), dec: AtomicU64::new(0) } }; SLOTS];

pub static MERGE_QUEUED: AtomicU64 = AtomicU64::new(0);
pub static STW_COUNT: AtomicU64 = AtomicU64::new(0);
pub static STW_WAIT_NS: AtomicU64 = AtomicU64::new(0);
pub static PARKED_NS: AtomicU64 = AtomicU64::new(0);
pub static GC_COUNT: AtomicU64 = AtomicU64::new(0);
pub static GC_NS: AtomicU64 = AtomicU64::new(0);
pub static QSBR_BATCHES: AtomicU64 = AtomicU64::new(0);
pub static QSBR_ITEMS: AtomicU64 = AtomicU64::new(0);
pub static MAX_THREADS: AtomicU64 = AtomicU64::new(0);
pub static SHARDED_FLUSHES: AtomicU64 = AtomicU64::new(0);
pub static SHARDED_MADE: AtomicU64 = AtomicU64::new(0);

fn slot_for(tp: usize) -> Option<&'static Slot> {
    let mut i = (tp.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 54) % SLOTS;
    for _ in 0..SLOTS {
        let s = &TYPES[i];
        let cur = s.tp.load(Relaxed);
        if cur == tp {
            return Some(s);
        }
        if cur == 0 {
            match s.tp.compare_exchange(0, tp, Relaxed, Relaxed) {
                Ok(_) => return Some(s),
                Err(x) if x == tp => return Some(s),
                Err(_) => {}
            }
        }
        i = (i + 1) % SLOTS;
    }
    None
}

#[cold]
pub fn note_nonowner(op: *mut crate::ffi::PyObject, inc: bool) {
    let tp = unsafe { (*op).type_ptr() } as usize;
    if let Some(s) = slot_for(tp) {
        if inc { s.inc.fetch_add(1, Relaxed) } else { s.dec.fetch_add(1, Relaxed) };
    }
}

pub fn add_ns(c: &AtomicU64, since: std::time::Instant) {
    c.fetch_add(since.elapsed().as_nanos() as u64, Relaxed);
}

extern "C" fn report() {
    use std::io::Write;
    let mut rows: Vec<(u64, u64, String)> = Vec::new();
    for s in TYPES.iter() {
        let tp = s.tp.load(Relaxed);
        if tp == 0 {
            continue;
        }
        let name = unsafe { crate::types::typeobject::type_name(&*(tp as *const crate::ffi::PyTypeObject)) }.to_string();
        rows.push((s.inc.load(Relaxed), s.dec.load(Relaxed), name));
    }
    rows.sort_by(|a, b| (b.0 + b.1).cmp(&(a.0 + a.1)));
    let mut e = std::io::stderr().lock();
    let _ = writeln!(e, "--- RPY_SCALESTATS (pid {}) ---", std::process::id());
    let _ = writeln!(e, "max threads {}  merge-queued {}  objects sharded {}  sharded-table flushes {}", MAX_THREADS.load(Relaxed),
        MERGE_QUEUED.load(Relaxed), SHARDED_MADE.load(Relaxed), SHARDED_FLUSHES.load(Relaxed));
    let _ = writeln!(e, "stop-the-world {}  wait {:.3} ms  parked (sum over threads) {:.3} ms",
        STW_COUNT.load(Relaxed), STW_WAIT_NS.load(Relaxed) as f64 / 1e6, PARKED_NS.load(Relaxed) as f64 / 1e6);
    let _ = writeln!(e, "collections {}  pause {:.3} ms", GC_COUNT.load(Relaxed), GC_NS.load(Relaxed) as f64 / 1e6);
    let _ = writeln!(e, "qsbr batches {}  items {}", QSBR_BATCHES.load(Relaxed), QSBR_ITEMS.load(Relaxed));
    report_phases(&mut e);
    let _ = writeln!(e, "non-owner refcount ops by type (incref / decref):");
    for (i, d, n) in rows.iter().take(25) {
        let _ = writeln!(e, "  {:>12} {:>12}  {}", i, d, n);
    }
}

// ---------------------------------------------------------------- (scale3) collector phases
//
// Thread CPU time and wall time of the collecting thread per collection phase, plus the
// time the world stayed stopped for collections and the number of objects examined /
// found unreachable. Printed with the report above.

pub const PHASES: [&str; 8] = ["pre-stop", "prepare", "snapshot", "analyse", "stopped-rest", "clear", "post", "helpers"];
pub static PHASE_CPU_NS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
pub static PHASE_WALL_NS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
pub static GC_STOPPED_NS: AtomicU64 = AtomicU64::new(0);
pub static GC_EXAMINED: AtomicU64 = AtomicU64::new(0);
pub static GC_GARBAGE: AtomicU64 = AtomicU64::new(0);

/// (scale3) `RPY_GCDEBUG` set: print one line per collection to stderr (diagnostics).
pub fn gc_debug() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var_os("RPY_GCDEBUG").is_some())
}

/// CPU time of the calling thread in nanoseconds.
pub fn thread_cpu_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Phase timer: `lap(i)` charges the time since the previous lap to phase `i`.
pub struct PhaseClock(Option<(u64, std::time::Instant)>);
impl PhaseClock {
    pub fn start() -> Self {
        PhaseClock(if on() { Some((thread_cpu_ns(), std::time::Instant::now())) } else { None })
    }
    pub fn lap(&mut self, i: usize) {
        if let Some((c, w)) = self.0 {
            let c2 = thread_cpu_ns();
            let w2 = std::time::Instant::now();
            PHASE_CPU_NS[i].fetch_add(c2.saturating_sub(c), Relaxed);
            PHASE_WALL_NS[i].fetch_add((w2 - w).as_nanos() as u64, Relaxed);
            self.0 = Some((c2, w2));
        }
    }
}

fn report_phases(e: &mut impl std::io::Write) {
    let _ = writeln!(e, "gcphase stopped {:.3} ms  examined {}  unreachable {}  parallel jobs {} helps {} (serial: {} no helper, {} small) handed {}", GC_STOPPED_NS.load(Relaxed) as f64 / 1e6,
        GC_EXAMINED.load(Relaxed), GC_GARBAGE.load(Relaxed), crate::gcpar::JOBS.load(Relaxed), crate::gcpar::HELPS.load(Relaxed), crate::gcpar::NO_HELPERS.load(Relaxed), crate::gcpar::TOO_SMALL.load(Relaxed), crate::gc::HANDED.load(Relaxed));
    let mut s = String::from("gcphase cpu/wall ms:");
    for (i, n) in PHASES.iter().enumerate() {
        s.push_str(&format!(" {}={:.1}/{:.1}", n, PHASE_CPU_NS[i].load(Relaxed) as f64 / 1e6, PHASE_WALL_NS[i].load(Relaxed) as f64 / 1e6));
    }
    let _ = writeln!(e, "{}", s);
}

/// Called once at runtime start-up.
pub fn init() {
    if std::env::var_os("RPY_SCALESTATS").is_some() {
        ON.store(true, Relaxed);
        unsafe { libc::atexit(report) };
    }
}
