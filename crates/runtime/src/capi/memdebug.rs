//! (capi3) Debug wrappers for the three C-API allocator domains (PYTHONMALLOC=debug,
//! `*_debug`, -X dev, PyMem_SetupDebugHooks).
//!
//! Every block handed out by a wrapper is surrounded by guard bytes:
//!
//! ```text
//!   base: [requested size: usize][domain letter: u8][7 x GUARD]
//!   p   : [data: n bytes, NEW_FILL on malloc][8 x GUARD]
//! ```
//!
//! On free/realloc the wrapper checks the guards and the domain letter and aborts the
//! process with a report of the block when they are wrong; freed data is overwritten with
//! DEAD_FILL first. The letters are 'r' (raw), 'm' (mem) and 'o' (object). The mem and
//! object wrappers also abort when called from a thread without an attached thread state.
//!
//! Our runtime allocates its own objects outside these domains, and C extensions release
//! them through the domains (`tp_free = PyObject_Free`). So the wrappers keep a set of the
//! blocks they produced, and a pointer outside that set is passed to the wrapped
//! allocator untouched instead of being treated as a corrupted debug block.

use super::mem::PyMemAllocatorEx;
use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::Mutex;

pub const GUARD: u8 = 0xFD;
pub const NEW_FILL: u8 = 0xCD;
pub const DEAD_FILL: u8 = 0xDD;
/// Bytes in front of the data: size word + letter + 7 guard bytes.
const HEAD: usize = 16;
/// Guard bytes after the data.
const TAIL: usize = 8;

pub struct DebugDomain {
    pub letter: u8,
    pub under: PyMemAllocatorEx,
}

const NULL_ALLOC: PyMemAllocatorEx =
    PyMemAllocatorEx { ctx: std::ptr::null_mut(), malloc: None, calloc: None, realloc: None, free: None };

pub static mut DOMAINS: [DebugDomain; 3] = [
    DebugDomain { letter: b'r', under: NULL_ALLOC },
    DebugDomain { letter: b'm', under: NULL_ALLOC },
    DebugDomain { letter: b'o', under: NULL_ALLOC },
];

const NSETS: usize = 16;
static LIVE: [Mutex<Option<HashSet<usize>>>; NSETS] = [const { Mutex::new(None) }; NSETS];

fn live_set(p: usize) -> std::sync::MutexGuard<'static, Option<HashSet<usize>>> {
    LIVE[(p >> 4) % NSETS].lock().unwrap_or_else(|e| e.into_inner())
}
fn remember(p: usize) {
    live_set(p).get_or_insert_with(HashSet::new).insert(p);
}
/// Forget `p`; true when it was a block of ours.
fn forget(p: usize) -> bool {
    live_set(p).as_mut().is_some_and(|s| s.remove(&p))
}
pub fn is_debug_block(p: *const c_void) -> bool {
    live_set(p as usize).as_ref().is_some_and(|s| s.contains(&(p as usize)))
}

fn wr(s: &str) {
    let b = s.as_bytes();
    let mut off = 0;
    while off < b.len() {
        let n = unsafe { libc::write(2, b[off..].as_ptr() as *const c_void, b.len() - off) };
        if n <= 0 {
            break;
        }
        off += n as usize;
    }
}

unsafe fn ctx_of(ctx: *mut c_void) -> &'static DebugDomain {
    unsafe { &*(ctx as *const DebugDomain) }
}

fn require_thread_state(func: &str, d: &DebugDomain) {
    if d.letter != b'r' && crate::threads::current_ptr().is_null() {
        super::refcount::fatal_with_func(
            func,
            "Python memory allocator called without an active thread state. \
             Are you trying to call it inside of a Py_BEGIN_ALLOW_THREADS block?",
        );
    }
}

/// Write the header and trailer around a fresh block of `n` data bytes at `base`.
unsafe fn frame_block(base: *mut u8, n: usize, letter: u8, fill: bool) -> *mut c_void {
    unsafe {
        (base as *mut usize).write_unaligned(n);
        *base.add(8) = letter;
        std::ptr::write_bytes(base.add(9), GUARD, HEAD - 9);
        let p = base.add(HEAD);
        if fill {
            std::ptr::write_bytes(p, NEW_FILL, n);
        }
        std::ptr::write_bytes(p.add(n), GUARD, TAIL);
        remember(p as usize);
        p as *mut c_void
    }
}

pub unsafe extern "C" fn debug_malloc(ctx: *mut c_void, n: usize) -> *mut c_void {
    unsafe {
        let d = ctx_of(ctx);
        require_thread_state("_PyMem_DebugMalloc", d);
        let Some(total) = n.checked_add(HEAD + TAIL) else { return std::ptr::null_mut() };
        let base = d.under.malloc.unwrap()(d.under.ctx, total) as *mut u8;
        if base.is_null() {
            return std::ptr::null_mut();
        }
        frame_block(base, n, d.letter, true)
    }
}

pub unsafe extern "C" fn debug_calloc(ctx: *mut c_void, nelem: usize, elsize: usize) -> *mut c_void {
    unsafe {
        let d = ctx_of(ctx);
        require_thread_state("_PyMem_DebugCalloc", d);
        let Some(n) = nelem.checked_mul(elsize) else { return std::ptr::null_mut() };
        let Some(total) = n.checked_add(HEAD + TAIL) else { return std::ptr::null_mut() };
        let base = d.under.calloc.unwrap()(d.under.ctx, 1, total) as *mut u8;
        if base.is_null() {
            return std::ptr::null_mut();
        }
        frame_block(base, n, d.letter, false)
    }
}

/// Verify a block of ours before it is released or resized; abort with a report when
/// it was damaged or belongs to another domain.
unsafe fn check_block(func: &str, d: &DebugDomain, p: *mut u8) {
    unsafe {
        let base = p.sub(HEAD);
        let n = (base as *const usize).read_unaligned();
        let letter = *base.add(8);
        if letter != d.letter {
            report(p);
            let msg = format!(
                "bad ID: Allocated using API '{}', verified using API '{}'",
                letter as char, d.letter as char
            );
            super::refcount::fatal_with_func(func, &msg);
        }
        if (9..HEAD).any(|i| *base.add(i) != GUARD) {
            report(p);
            super::refcount::fatal_with_func(func, "bad leading pad byte");
        }
        if (0..TAIL).any(|i| *p.add(n + i) != GUARD) {
            report(p);
            super::refcount::fatal_with_func(func, "bad trailing pad byte");
        }
    }
}

/// Describe a debug block on stderr (size, domain, guards, first/last data bytes).
unsafe fn report(p: *mut u8) {
    unsafe {
        let base = p.sub(HEAD);
        let n = (base as *const usize).read_unaligned();
        let letter = *base.add(8);
        wr(&format!("Debug memory block at address p={:p}: API '{}'\n", p, letter as char));
        wr(&format!("    {} bytes originally requested\n", n));
        let lead = HEAD - 9;
        let lead_bad: Vec<usize> = (9..HEAD).filter(|&i| *base.add(i) != GUARD).collect();
        if lead_bad.is_empty() {
            wr(&format!("    The {} pad bytes at p-{} are FORBIDDENBYTE, as expected.\n", lead, lead));
        } else {
            wr(&format!("    The {} pad bytes at p-{} are not all FORBIDDENBYTE (0x{:02x}):\n", lead, lead, GUARD));
            for i in 9..HEAD {
                let b = *base.add(i);
                let mark = if b != GUARD { " *** OUCH" } else { "" };
                wr(&format!("        at p-{}: 0x{:02x}{}\n", HEAD - i, b, mark));
            }
        }
        let tail = p.add(n);
        if (0..TAIL).all(|i| *tail.add(i) == GUARD) {
            wr(&format!("    The {} pad bytes at tail={:p} are FORBIDDENBYTE, as expected.\n", TAIL, tail));
        } else {
            wr(&format!("    The {} pad bytes at tail={:p} are not all FORBIDDENBYTE (0x{:02x}):\n", TAIL, tail, GUARD));
            for i in 0..TAIL {
                let b = *tail.add(i);
                let mark = if b != GUARD { " *** OUCH" } else { "" };
                wr(&format!("        at tail+{}: 0x{:02x}{}\n", i, b, mark));
            }
        }
        if n > 0 {
            let hex = |r: std::ops::Range<usize>| r.map(|i| format!("{:02x}", *p.add(i))).collect::<Vec<_>>().join(" ");
            let line = if n <= 16 { hex(0..n) } else { format!("{} ... {}", hex(0..8), hex(n - 8..n)) };
            wr(&format!("    Data at p: {}\n", line));
        }
        wr("\n");
        if crate::modules::_tracemalloc::TRACING.load(std::sync::atomic::Ordering::Relaxed) {
            wr("tracemalloc keeps no allocation traceback for this block\n");
        } else {
            wr("Enable tracemalloc to get the memory block allocation traceback\n");
        }
        wr("\n");
    }
}

pub unsafe extern "C" fn debug_free(ctx: *mut c_void, p: *mut c_void) {
    unsafe {
        let d = ctx_of(ctx);
        if p.is_null() {
            return;
        }
        if !is_debug_block(p) {
            d.under.free.unwrap()(d.under.ctx, p);
            return;
        }
        check_block("_PyMem_DebugRawFree", d, p as *mut u8);
        forget(p as usize);
        let base = (p as *mut u8).sub(HEAD);
        let n = (base as *const usize).read_unaligned();
        std::ptr::write_bytes(base, DEAD_FILL, HEAD + n + TAIL);
        d.under.free.unwrap()(d.under.ctx, base as *mut c_void);
    }
}

pub unsafe extern "C" fn debug_realloc(ctx: *mut c_void, p: *mut c_void, n: usize) -> *mut c_void {
    unsafe {
        let d = ctx_of(ctx);
        if p.is_null() {
            return debug_malloc(ctx, n);
        }
        require_thread_state("_PyMem_DebugRealloc", d);
        if !is_debug_block(p) {
            return d.under.realloc.unwrap()(d.under.ctx, p, n);
        }
        check_block("_PyMem_DebugRawRealloc", d, p as *mut u8);
        let Some(total) = n.checked_add(HEAD + TAIL) else { return std::ptr::null_mut() };
        let base = (p as *mut u8).sub(HEAD);
        let old = (base as *const usize).read_unaligned();
        // the old trailer would be copied into the new data: wipe it first
        std::ptr::write_bytes(base.add(HEAD + old), DEAD_FILL, TAIL);
        forget(p as usize);
        let nb = d.under.realloc.unwrap()(d.under.ctx, base as *mut c_void, total) as *mut u8;
        if nb.is_null() {
            // the old block is still valid: restore its trailer
            std::ptr::write_bytes(base.add(HEAD + old), GUARD, TAIL);
            remember(p as usize);
            return std::ptr::null_mut();
        }
        let np = frame_block(nb, n, d.letter, false);
        if n > old {
            std::ptr::write_bytes((np as *mut u8).add(old), NEW_FILL, n - old);
        }
        np
    }
}

pub fn is_debug_wrapper(a: &PyMemAllocatorEx) -> bool {
    let m: unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void = debug_malloc;
    a.malloc.is_some_and(|f| f as usize == m as usize)
}

/// The allocator a debug wrapper `a` forwards to.
pub unsafe fn wrapped(a: &PyMemAllocatorEx) -> PyMemAllocatorEx {
    unsafe { ctx_of(a.ctx).under }
}

/// Wrap domain `d` (currently `cur`) with the debug hooks; returns the new allocator.
pub unsafe fn wrapper_for(d: usize, cur: PyMemAllocatorEx) -> PyMemAllocatorEx {
    unsafe {
        let dd = &mut (*(&raw mut DOMAINS))[d];
        dd.under = cur;
        PyMemAllocatorEx {
            ctx: dd as *mut DebugDomain as *mut c_void,
            malloc: Some(debug_malloc),
            calloc: Some(debug_calloc),
            realloc: Some(debug_realloc),
            free: Some(debug_free),
        }
    }
}

/// `_PyObject_IsFreed`-style probe: does the type slot of `op` hold one of the fill
/// patterns the debug hooks write (fresh, released or guard memory), or is `op` null?
pub unsafe fn object_looks_freed(op: *const crate::ffi::PyObject) -> bool {
    if op.is_null() {
        return true;
    }
    unsafe {
        let off = std::mem::offset_of!(crate::ffi::PyObject, ob_type);
        let tp = ((op as *const u8).add(off) as *const usize).read_unaligned();
        [NEW_FILL, DEAD_FILL, GUARD].iter().any(|&b| tp == usize::from_ne_bytes([b; 8])) || tp == 0
    }
}
