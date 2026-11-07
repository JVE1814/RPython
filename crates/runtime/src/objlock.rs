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

/// Lock `o` for the calling thread (recursively). No-op while single-threaded.
pub fn lock(o: &PyObject) -> ObjGuard {
    if !crate::threads::multi_threaded() {
        return ObjGuard { key: 0 };
    }
    let key = o.as_ptr() as usize;
    let me = crate::object::thread_id();
    let s = shard(key);
    let mut g = guard(&s.held);
    loop {
        match g.iter_mut().find(|e| e.key == key) {
            None => {
                g.push(Entry { key, owner: me, depth: 1 });
                return ObjGuard { key };
            }
            Some(e) if e.owner == me => {
                e.depth += 1;
                return ObjGuard { key };
            }
            Some(_) => {
                s.waiters.fetch_add(1, SeqCst);
                // wait as a blocked thread: the owner may need a stop-the-world to
                // finish before it can release the entry
                let ts = crate::threads::current();
                let prev = ts.mode.replace(crate::threads::ThreadMode::Blocked);
                let (g2, _) = s.freed.wait_timeout(g, std::time::Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner());
                drop(g2);
                s.waiters.fetch_sub(1, SeqCst);
                // re-entering the VM may wait for a stopped world: not while holding
                // the shard lock
                ts.mode.set(prev);
                g = guard(&s.held);
            }
        }
    }
}

/// Run `f` with `o` locked.
#[inline]
pub fn with<R>(o: &PyObject, f: impl FnOnce() -> R) -> R {
    let _g = lock(o);
    f()
}

fn unlock(key: usize) {
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
