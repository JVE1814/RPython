//! Executable memory with W^X: code is written through a writable view and executed
//! through a separate read+execute view of the same physical pages (memfd mapped
//! twice), so no page is ever writable and executable at the same time and new code
//! can be packed next to code other threads are running.
//! Fallback (no memfd): one private mapping per chunk, flipped RW -> RX with mprotect
//! before publication (never written again).
//!
//! (perf8) Code cache near the executable, as HotSpot reserves its code cache once at
//! start: the first chunk is a 256 MB reservation (a sparse memfd: only pages written
//! cost memory) whose executable view is placed within +-1 GB of the executable's text,
//! so compiled code reaches every runtime helper and static with a rel32 displacement
//! (`call rel32`, 5 bytes, instead of `movabs rax, imm64; call rax`, 12; RIP-relative
//! `lea` for static addresses). `near_window` tells the assembler which targets qualify;
//! `install` refuses code whose rel32 fixups do not reach from where it would land (the
//! reservation is full: later chunks are wherever mmap puts them).
//!
//! Freed blocks go to a simple size-segregated free list.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::*};

const CHUNK: usize = 4 << 20;
const ALIGN: usize = 64;
/// (perf8) size of the near reservation
const RESERVE: usize = 256 << 20;
/// (perf8) the near view lies within this distance of the anchor (a function of the
/// executable); targets within the same distance of the anchor are reachable from it
const NEAR_SPAN: usize = 1 << 30;

struct Chunk {
    rw: usize,
    rx: usize,
    size: usize,
    used: usize,
}

struct Heap {
    chunks: Vec<Chunk>,
    /// free blocks: (rx address, size)
    free: Vec<(usize, usize)>,
    dual: Option<bool>,
}

static HEAP: Mutex<Heap> = Mutex::new(Heap { chunks: Vec::new(), free: Vec::new(), dual: None });
/// (perf8) rx range of the near reservation ([NEAR_LO, NEAR_HI); 0/0: none)
static NEAR_LO: AtomicUsize = AtomicUsize::new(0);
static NEAR_HI: AtomicUsize = AtomicUsize::new(0);
/// 0 = not initialised, 1 = initialised
static READY: AtomicUsize = AtomicUsize::new(0);

fn memfd(size: usize) -> Option<i32> {
    unsafe {
        let fd = libc::syscall(libc::SYS_memfd_create, c"rpython-jit".as_ptr(), libc::MFD_CLOEXEC as libc::c_uint) as i32;
        if fd < 0 {
            return None;
        }
        if libc::ftruncate(fd, size as libc::off_t) != 0 {
            libc::close(fd);
            return None;
        }
        Some(fd)
    }
}

fn new_chunk_dual(size: usize) -> Option<Chunk> {
    unsafe {
        let fd = memfd(size)?;
        let rw = libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
        if rw == libc::MAP_FAILED {
            libc::close(fd);
            return None;
        }
        let rx = libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_EXEC, libc::MAP_SHARED, fd, 0);
        libc::close(fd);
        if rx == libc::MAP_FAILED {
            libc::munmap(rw, size);
            return None;
        }
        Some(Chunk { rw: rw as usize, rx: rx as usize, size, used: 0 })
    }
}

/// An address inside the executable's text.
fn anchor() -> usize {
    new_chunk_near as fn() -> Option<Chunk> as usize
}

/// (perf8) The reservation with its executable view near the executable: hints below
/// the executable first (the heap grows above it), then above; a mapping the kernel
/// placed elsewhere is given back. RPYTHON_JIT_FAR=1 skips this (long call forms).
fn new_chunk_near() -> Option<Chunk> {
    if std::env::var_os("RPYTHON_JIT_FAR").is_some_and(|v| v != "0") {
        return None;
    }
    let a = anchor();
    unsafe {
        let fd = memfd(RESERVE)?;
        let rw = libc::mmap(std::ptr::null_mut(), RESERVE, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
        if rw == libc::MAP_FAILED {
            libc::close(fd);
            return None;
        }
        let step = 64 << 20;
        let mut hints: Vec<usize> = Vec::new();
        for k in 0..10usize {
            // below: the view ends 64 MB + k steps under the anchor's 2 MB block
            if let Some(h) = (a & !0x1f_ffff).checked_sub(RESERVE + step + k * step) {
                hints.push(h);
            }
        }
        for k in 0..6usize {
            hints.push((a & !0x1f_ffff) + 2 * step + k * step);
        }
        for h in hints {
            let rx = libc::mmap(h as *mut libc::c_void, RESERVE, libc::PROT_READ | libc::PROT_EXEC, libc::MAP_SHARED, fd, 0);
            if rx == libc::MAP_FAILED {
                continue;
            }
            let (lo, hi) = (rx as usize, rx as usize + RESERVE);
            if lo.abs_diff(a) < NEAR_SPAN && hi.abs_diff(a) < NEAR_SPAN {
                libc::close(fd);
                return Some(Chunk { rw: rw as usize, rx: lo, size: RESERVE, used: 0 });
            }
            libc::munmap(rx, RESERVE);
        }
        libc::close(fd);
        libc::munmap(rw, RESERVE);
        None
    }
}

#[cold]
fn init_locked(h: &mut Heap) {
    if h.dual.is_some() {
        return;
    }
    if let Some(c) = new_chunk_near() {
        NEAR_LO.store(c.rx, Relaxed);
        NEAR_HI.store(c.rx + c.size, Relaxed);
        h.chunks.push(c);
        h.dual = Some(true);
    } else {
        let c = new_chunk_dual(CHUNK);
        h.dual = Some(c.is_some());
        if let Some(c) = c {
            h.chunks.push(c);
        }
    }
    READY.store(1, Release);
}

/// (perf8) Range of absolute targets that code in the near reservation reaches with a
/// rel32 displacement, or None (no reservation: code uses 64-bit forms).
#[inline]
pub fn near_window() -> Option<(usize, usize)> {
    if READY.load(Acquire) == 0 {
        init_locked(&mut HEAP.lock().unwrap());
    }
    let (lo, hi) = (NEAR_LO.load(Relaxed), NEAR_HI.load(Relaxed));
    if lo == 0 {
        return None;
    }
    // every byte of the reservation reaches [hi - 2^31, lo + 2^31) with margin
    const M: usize = (1 << 31) - 4096;
    Some((hi.saturating_sub(M), lo + M))
}

/// (perf8) Whether `target` is reachable with rel32 from anywhere in the near reservation.
#[inline]
pub fn reachable(target: usize) -> bool {
    match near_window() {
        Some((lo, hi)) => target >= lo && target < hi,
        None => false,
    }
}

/// Copy `code` into executable memory; `patch(buf, rx_address)` fixes up absolute and
/// rel32-to-absolute references before the code becomes executable and returns false
/// if one does not reach from that address (the code is then not installed). Returns
/// the executable address.
pub fn install(code: &[u8], patch: impl FnOnce(&mut [u8], usize) -> bool) -> Option<usize> {
    let size = (code.len() + ALIGN - 1) & !(ALIGN - 1);
    let mut h = HEAP.lock().unwrap();
    if h.dual.is_none() {
        init_locked(&mut h);
    }
    if h.dual == Some(true) {
        // reuse a free block (first fit) or bump-allocate
        let mut target: Option<(usize, usize)> = None; // (rw, rx)
        let mut reused = None;
        if let Some(i) = h.free.iter().position(|&(_, s)| s >= size && s <= size * 2) {
            let (rx, s) = h.free.swap_remove(i);
            let c = h.chunks.iter().find(|c| rx >= c.rx && rx < c.rx + c.size).unwrap();
            target = Some((c.rw + (rx - c.rx), rx));
            reused = Some((rx, s));
        }
        if target.is_none() {
            let need_new = h.chunks.last().is_none_or(|c| c.used + size > c.size);
            if need_new {
                let c = new_chunk_dual(size.max(CHUNK))?;
                h.chunks.push(c);
            }
            let c = h.chunks.last_mut().unwrap();
            let off = c.used;
            c.used += size;
            target = Some((c.rw + off, c.rx + off));
        }
        let (rw, rx) = target.unwrap();
        unsafe {
            let dst = std::slice::from_raw_parts_mut(rw as *mut u8, code.len());
            dst.copy_from_slice(code);
            if !patch(dst, rx) {
                // (perf8) a rel32 reference does not reach from here (the near
                // reservation is full): give the block back
                h.free.push(reused.unwrap_or((rx, size)));
                return None;
            }
            // x86: instruction fetch is coherent with data writes through another mapping;
            // publication to other threads happens with release/acquire on the code pointer.
        }
        return Some(rx);
    }
    drop(h);
    // fallback: private mapping, written then made RX
    unsafe {
        let len = (code.len() + 4095) & !4095;
        let p = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0);
        if p == libc::MAP_FAILED {
            return None;
        }
        let dst = std::slice::from_raw_parts_mut(p as *mut u8, code.len());
        dst.copy_from_slice(code);
        if !patch(dst, p as usize) || libc::mprotect(p, len, libc::PROT_READ | libc::PROT_EXEC) != 0 {
            libc::munmap(p, len);
            return None;
        }
        Some(p as usize)
    }
}

/// (perf8) Install the assembled code of `a` (resolved), applying its absolute and
/// rel32-to-absolute fixups for the address it lands at.
pub fn install_asm(a: &super::x64::Asm) -> Option<usize> {
    install(&a.buf, |buf, base| a.patch_abs(buf, base).is_ok())
}

fn under_valgrind() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("LD_PRELOAD").is_ok_and(|p| p.contains("vgpreload")) || std::env::var_os("RPYTHON_JIT_NOREUSE").is_some())
}

/// Return code memory (the caller guarantees no thread can still execute it).
pub fn free(rx: usize, len: usize) {
    let size = (len + ALIGN - 1) & !(ALIGN - 1);
    let mut h = HEAP.lock().unwrap();
    if h.dual == Some(true) && h.chunks.iter().any(|c| rx >= c.rx && rx < c.rx + c.size) {
        // (jit6) under valgrind, code written through the other mapping into a block
        // it already translated would run stale translations: no reuse there
        if !under_valgrind() {
            h.free.push((rx, size));
        }
        return;
    }
    drop(h);
    unsafe {
        libc::munmap(rx as *mut libc::c_void, (len + 4095) & !4095);
    }
}

/// In a forked child: the code chunks are MAP_SHARED views of a memfd that
/// the parent keeps using, so code installed later by either process would
/// overwrite live code of the other.  Give the child private copies, mapped
/// at the same addresses (compiled code pointers stay valid).
pub fn after_fork_child() {
    let h = match HEAP.try_lock() {
        Ok(h) => h,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        // held by a thread that no longer exists: leave it (the world is
        // stopped before fork, so this does not happen in practice)
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    for c in h.chunks.iter() {
        unsafe {
            let Some(fd) = memfd(c.size) else { continue };
            let tmp = libc::mmap(std::ptr::null_mut(), c.size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            if tmp == libc::MAP_FAILED {
                libc::close(fd);
                continue;
            }
            // (perf8) only the part handed out so far: the near reservation is sparse
            // and reading the rest would allocate it
            std::ptr::copy_nonoverlapping(c.rw as *const u8, tmp as *mut u8, c.used);
            libc::mmap(c.rw as *mut libc::c_void, c.size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED | libc::MAP_FIXED, fd, 0);
            libc::mmap(c.rx as *mut libc::c_void, c.size, libc::PROT_READ | libc::PROT_EXEC, libc::MAP_SHARED | libc::MAP_FIXED, fd, 0);
            libc::munmap(tmp, c.size);
            libc::close(fd);
        }
    }
}
