//! Executable memory with W^X: code is written through a writable view and executed
//! through a separate read+execute view of the same physical pages (memfd mapped
//! twice), so no page is ever writable and executable at the same time and new code
//! can be packed next to code other threads are running.
//! Fallback (no memfd): one private mapping per chunk, flipped RW -> RX with mprotect
//! before publication (never written again).
//!
//! Freed blocks go to a simple size-segregated free list.

use std::sync::Mutex;

const CHUNK: usize = 4 << 20;
const ALIGN: usize = 64;

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

fn new_chunk_dual(size: usize) -> Option<Chunk> {
    unsafe {
        let fd = libc::syscall(libc::SYS_memfd_create, c"rpython-jit".as_ptr(), libc::MFD_CLOEXEC as libc::c_uint) as i32;
        if fd < 0 {
            return None;
        }
        if libc::ftruncate(fd, size as libc::off_t) != 0 {
            libc::close(fd);
            return None;
        }
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

/// Copy `code` into executable memory; `patch(buf, rx_address)` may fix up absolute
/// addresses before the code becomes executable. Returns the executable address.
pub fn install(code: &[u8], patch: impl FnOnce(&mut [u8], usize)) -> Option<usize> {
    let size = (code.len() + ALIGN - 1) & !(ALIGN - 1);
    let mut h = HEAP.lock().unwrap();
    if h.dual.is_none() {
        let c = new_chunk_dual(CHUNK);
        h.dual = Some(c.is_some());
        if let Some(c) = c {
            h.chunks.push(c);
        }
    }
    if h.dual == Some(true) {
        // reuse a free block (first fit) or bump-allocate
        let mut target: Option<(usize, usize)> = None; // (rw, rx)
        if let Some(i) = h.free.iter().position(|&(_, s)| s >= size && s <= size * 2) {
            let (rx, _) = h.free.swap_remove(i);
            let c = h.chunks.iter().find(|c| rx >= c.rx && rx < c.rx + c.size).unwrap();
            target = Some((c.rw + (rx - c.rx), rx));
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
            patch(dst, rx);
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
        patch(dst, p as usize);
        if libc::mprotect(p, len, libc::PROT_READ | libc::PROT_EXEC) != 0 {
            libc::munmap(p, len);
            return None;
        }
        Some(p as usize)
    }
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
            let fd = libc::syscall(libc::SYS_memfd_create, c"rpython-jit".as_ptr(), libc::MFD_CLOEXEC as libc::c_uint) as i32;
            if fd < 0 {
                continue;
            }
            if libc::ftruncate(fd, c.size as libc::off_t) != 0 {
                libc::close(fd);
                continue;
            }
            let tmp = libc::mmap(std::ptr::null_mut(), c.size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            if tmp == libc::MAP_FAILED {
                libc::close(fd);
                continue;
            }
            std::ptr::copy_nonoverlapping(c.rw as *const u8, tmp as *mut u8, c.size);
            libc::mmap(c.rw as *mut libc::c_void, c.size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED | libc::MAP_FIXED, fd, 0);
            libc::mmap(c.rx as *mut libc::c_void, c.size, libc::PROT_READ | libc::PROT_EXEC, libc::MAP_SHARED | libc::MAP_FIXED, fd, 0);
            libc::munmap(tmp, c.size);
            libc::close(fd);
        }
    }
}
