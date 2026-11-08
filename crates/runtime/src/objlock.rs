//! Per-object recursive locks for runtime objects whose Rust-side state is changed by
//! operations that can run Python code or block in a system call while the state is
//! in use (buffered/text I/O objects, array, OrderedDict, itertools iterators, ...).
//!
//! `ob_mutex` is a leaf lock: code holding it never runs Python code, never blocks and
//! never takes another object's lock. That rules it out for these objects, so they get
//! an entry in a separate table keyed by object address instead:
//! * recursive for the owning thread (a method that calls another method of the same
//!   object through Python, a finalizer printing to the stream it interrupted);
//! * a thread waiting for an entry counts as blocked for stop-the-world purposes, so a
//!   holder parked at a safepoint cannot deadlock the collector;
//! * held across blocking calls (unlike a critical section, it is not given up while
//!   the holder waits in a system call: a buffered writer's state must not change in
//!   the middle of a flush);
//! * two kinds: `lock` entries may be given up when their holder has to wait for
//!   another object's entry (and are taken back in address order: no lock-order
//!   deadlock, see `lock`); `lock_held` entries never are, for objects whose Rust state
//!   is borrowed across the Python calls a method makes (I/O, iterators, parsers,
//!   array, OrderedDict...). Deadlock remains possible only through cycles of held
//!   entries. `lock` is used where state is re-read after every Python call (deque,
//!   lru_cache, heapq);
//! * no cost while the process has a single thread (no entry is taken).
//! After fork() the child drops the entries of threads that do not exist there.

use crate::ffi::PyObject;
use std::sync::atomic::{AtomicUsize, Ordering::*};
use std::sync::{Condvar, Mutex, MutexGuard};

struct Entry {
    key: usize,
    owner: usize,
    depth: u32,
}

struct Shard {
    held: Mutex<Vec<Entry>>,
    freed: Condvar,
    waiters: AtomicUsize,
}

const NSHARDS: usize = 64;
static SHARDS: [Shard; NSHARDS] =
    [const { Shard { held: Mutex::new(Vec::new()), freed: Condvar::new(), waiters: AtomicUsize::new(0) } }; NSHARDS];

fn shard(key: usize) -> &'static Shard {
    &SHARDS[(key >> 4).wrapping_mul(0x9E37_79B9) % NSHARDS]
}

fn guard(m: &'static Mutex<Vec<Entry>>) -> MutexGuard<'static, Vec<Entry>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Holds an object lock until dropped.
pub struct ObjGuard {
    key: usize,
}

impl Drop for ObjGuard {
    fn drop(&mut self) {
        if self.key != 0 {
            unlock(self.key);
        }
    }
}

thread_local! {
    /// The calling thread's acquisitions, innermost last: (key, held across blocking).
    static HELD: std::cell::RefCell<Vec<(usize, bool)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Lock `o` for the calling thread (recursively). No-op while single-threaded.
///
/// Deadlock avoidance: a thread that finds the lock taken by another thread first
/// gives up every lock it holds that may be suspended (all but the `lock_held` ones),
/// then takes the whole set back, together with the new one, in address order. Two
/// threads locking two objects in opposite orders therefore cannot wait for each other
/// forever: the one that blocks lets go of what the other needs. (As with CPython's
/// critical sections, an object's state can change while code that locked it is
/// suspended this way, i.e. inside a nested call that had to wait for another lock.)
pub fn lock(o: &PyObject) -> ObjGuard {
    lock_impl(o, false)
}

/// As `lock`, but the lock is never given up to avoid a deadlock (buffered/text I/O:
/// the object's state must not change in the middle of an operation such as a flush).
pub fn lock_held(o: &PyObject) -> ObjGuard {
    lock_impl(o, true)
}

/// (scale3) `lock_held` for iterator slots: no lock when the caller's reference is the
/// only one and belongs to this thread (`object::exclusively_owned`, e.g. the iterator of
/// a running `for` loop): no other thread can reach the object then, and the slot
/// does not hand it to Python code. Shared iterators are locked as before.
#[inline]
pub fn lock_held_unless_exclusive(o: &PyObject) -> ObjGuard {
    if crate::object::exclusively_owned(o) {
        return ObjGuard { key: 0 };
    }
    lock_impl(o, true)
}

fn lock_impl(o: &PyObject, hold: bool) -> ObjGuard {
    if !crate::threads::multi_threaded() {
        return ObjGuard { key: 0 };
    }
    let key = o.as_ptr() as usize;
    let me = crate::object::thread_id();
    if !try_take(key, me, 1) {
        contended(key, me);
    }
    let _ = HELD.try_with(|h| h.borrow_mut().push((key, hold)));
    ObjGuard { key }
}

/// Take `key` (or add `depth` to our own entry); false if another thread holds it.
fn try_take(key: usize, me: usize, depth: u32) -> bool {
    let s = shard(key);
    let mut g = guard(&s.held);
    match g.iter_mut().find(|e| e.key == key) {
        None => {
            g.push(Entry { key, owner: me, depth });
            true
        }
        Some(e) if e.owner == me => {
            e.depth += depth;
            true
        }
        Some(_) => false,
    }
}

/// Wait until `key` is free (or ours), as a blocked thread: the owner may need a
/// stop-the-world to finish before it can release the entry.
fn wait_free(key: usize, me: usize) {
    let s = shard(key);
    let mut g = guard(&s.held);
    while g.iter().any(|e| e.key == key && e.owner != me) {
        s.waiters.fetch_add(1, SeqCst);
        let ts = crate::threads::current_ptr();
        let prev = if ts.is_null() { None } else { Some(unsafe { (*ts).mode.replace(crate::threads::ThreadMode::Blocked) }) };
        let (g2, _) = s.freed.wait_timeout(g, std::time::Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner());
        drop(g2);
        s.waiters.fetch_sub(1, SeqCst);
        // re-entering the VM may wait for a stopped world: not while holding the shard lock
        if let Some(p) = prev {
            unsafe { (*ts).mode.set(p) };
        }
        g = guard(&s.held);
    }
}

/// Drop our entry for `key` entirely; returns its depth.
fn release_entry(key: usize) -> u32 {
    let s = shard(key);
    let mut g = guard(&s.held);
    let mut depth = 0;
    if let Some(i) = g.iter().position(|e| e.key == key) {
        depth = g[i].depth;
        g.swap_remove(i);
    }
    drop(g);
    if s.waiters.load(SeqCst) != 0 {
        s.freed.notify_all();
    }
    depth
}

#[cold]
fn contended(key: usize, me: usize) {
    // the locks this thread may give up: held, and never taken with lock_held
    let mut suspend: Vec<usize> = Vec::new();
    let _ = HELD.try_with(|h| {
        let h = h.borrow();
        for &(k, _) in h.iter() {
            if k != key && !suspend.contains(&k) && !h.iter().any(|&(k2, hold)| k2 == k && hold) {
                suspend.push(k);
            }
        }
    });
    if suspend.is_empty() {
        loop {
            wait_free(key, me);
            if try_take(key, me, 1) {
                return;
            }
        }
    }
    let mut set: Vec<(usize, u32)> = suspend.iter().map(|&k| (k, release_entry(k))).filter(|&(_, d)| d > 0).collect();
    set.push((key, 1));
    set.sort_unstable_by_key(|&(k, _)| k);
    for (k, depth) in set {
        loop {
            if try_take(k, me, depth) {
                break;
            }
            wait_free(k, me);
        }
    }
}


/// Define `extern "C" fn $name(self, args...)` that calls slot function `$f` with
/// `self` locked: `crate::locked_slot!(l_len, my_len, () -> isize);`
#[macro_export]
macro_rules! locked_slot {
    ($name:ident, $f:path, ($($arg:ident: $t:ty),*) -> $ret:ty) => {
        unsafe extern "C" fn $name(o: *mut $crate::ffi::PyObject $(, $arg: $t)*) -> $ret {
            unsafe {
                let _g = $crate::objlock::lock_held(&*o);
                $f(o $(, $arg)*)
            }
        }
    };
}

/// (scale3) `locked_slot!` with `lock_held_unless_exclusive` (iterator slots).
#[macro_export]
macro_rules! locked_iter_slot {
    ($name:ident, $f:path, ($($arg:ident: $t:ty),*) -> $ret:ty) => {
        unsafe extern "C" fn $name(o: *mut $crate::ffi::PyObject $(, $arg: $t)*) -> $ret {
            unsafe {
                let _g = $crate::objlock::lock_held_unless_exclusive(&*o);
                $f(o $(, $arg)*)
            }
        }
    };
}

/// Run `f` with `o` locked.
#[inline]
pub fn with<R>(o: &PyObject, f: impl FnOnce() -> R) -> R {
    let _g = lock(o);
    f()
}

fn unlock(key: usize) {
    let _ = HELD.try_with(|h| {
        let mut h = h.borrow_mut();
        if let Some(i) = h.iter().rposition(|&(k, _)| k == key) {
            h.remove(i);
        }
    });
    let s = shard(key);
    let mut g = guard(&s.held);
    if let Some(i) = g.iter().position(|e| e.key == key) {
        let e = &mut g[i];
        e.depth -= 1;
        if e.depth == 0 {
            g.swap_remove(i);
            drop(g);
            if s.waiters.load(SeqCst) != 0 {
                s.freed.notify_all();
            }
        }
    }
}

/// Is `o` locked by the calling thread?
pub fn held_by_me(o: &PyObject) -> bool {
    let key = o.as_ptr() as usize;
    let me = crate::object::thread_id();
    guard(&shard(key).held).iter().any(|e| e.key == key && e.owner == me)
}

/// In a forked child: entries of threads that did not survive the fork are dropped.
pub fn after_fork_child() {
    let me = crate::object::thread_id();
    for s in SHARDS.iter() {
        if let Ok(mut g) = s.held.try_lock() {
            g.retain(|e| e.owner == me);
        }
    }
}
