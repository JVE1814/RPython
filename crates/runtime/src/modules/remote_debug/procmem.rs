//! (remote1) Access to another process's memory (Linux): `process_vm_readv` /
//! `process_vm_writev`, `/proc/PID/maps`, and locating the debug table through the ELF
//! section headers of the target's executable.

use std::io;

/// Why a read failed: the process is gone, we may not look at it, or the address was
/// not mapped / the data made no sense (transient while the target runs).
#[derive(Debug)]
pub enum MemError {
    NoProcess,
    Permission(String),
    Fault(String),
    Os(io::Error),
}

impl MemError {
    pub fn from_errno(e: io::Error, what: &str) -> MemError {
        match e.raw_os_error() {
            Some(libc::ESRCH) => MemError::NoProcess,
            Some(libc::EPERM) | Some(libc::EACCES) => MemError::Permission(format!("{}: {}", what, e)),
            Some(libc::EFAULT) | Some(libc::EIO) => MemError::Fault(format!("{} failed: {}", what, e)),
            _ => MemError::Os(e),
        }
    }
}

pub type MemResult<T> = Result<T, MemError>;

pub struct Process {
    pub pid: i32,
}

impl Process {
    pub fn new(pid: i32) -> Process {
        Process { pid }
    }

    /// Is the process still there (and not a zombie)?
    pub fn alive(&self) -> bool {
        match std::fs::read_to_string(format!("/proc/{}/stat", self.pid)) {
            Ok(s) => match s.rfind(')') {
                Some(i) => !s[i + 1..].trim_start().starts_with('Z'),
                None => true,
            },
            Err(_) => false,
        }
    }

    /// Copy `buf.len()` bytes from `addr` in the target.
    pub fn read(&self, addr: u64, buf: &mut [u8]) -> MemResult<()> {
        if buf.is_empty() {
            return Ok(());
        }
        if addr == 0 {
            return Err(MemError::Fault("read of a NULL address".into()));
        }
        let mut done = 0usize;
        while done < buf.len() {
            let local = libc::iovec { iov_base: buf[done..].as_mut_ptr() as *mut libc::c_void, iov_len: buf.len() - done };
            let remote = libc::iovec { iov_base: (addr as usize + done) as *mut libc::c_void, iov_len: buf.len() - done };
            let n = unsafe { libc::process_vm_readv(self.pid, &local, 1, &remote, 1, 0) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EFAULT) && !self.alive() {
                    return Err(MemError::NoProcess);
                }
                return Err(MemError::from_errno(e, "process_vm_readv"));
            }
            if n == 0 {
                return Err(MemError::Fault("short read from the target process".into()));
            }
            done += n as usize;
        }
        Ok(())
    }

    pub fn write(&self, addr: u64, data: &[u8]) -> MemResult<()> {
        let mut done = 0usize;
        while done < data.len() {
            let local = libc::iovec { iov_base: data[done..].as_ptr() as *mut libc::c_void, iov_len: data.len() - done };
            let remote = libc::iovec { iov_base: (addr as usize + done) as *mut libc::c_void, iov_len: data.len() - done };
            let n = unsafe { libc::process_vm_writev(self.pid, &local, 1, &remote, 1, 0) };
            if n <= 0 {
                return Err(MemError::from_errno(io::Error::last_os_error(), "process_vm_writev"));
            }
            done += n as usize;
        }
        Ok(())
    }

    pub fn read_vec(&self, addr: u64, len: usize) -> MemResult<Vec<u8>> {
        let mut v = vec![0u8; len];
        self.read(addr, &mut v)?;
        Ok(v)
    }
    pub fn u64(&self, addr: u64) -> MemResult<u64> {
        let mut b = [0u8; 8];
        self.read(addr, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }
    pub fn u32(&self, addr: u64) -> MemResult<u32> {
        let mut b = [0u8; 4];
        self.read(addr, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }
    pub fn u8(&self, addr: u64) -> MemResult<u8> {
        let mut b = [0u8; 1];
        self.read(addr, &mut b)?;
        Ok(b[0])
    }
}

/// Little-endian field accessors over a copied block.
pub fn rd_u64(b: &[u8], off: u64) -> u64 {
    let o = off as usize;
    b.get(o..o + 8).map_or(0, |s| u64::from_le_bytes(s.try_into().unwrap()))
}
pub fn rd_u32(b: &[u8], off: u64) -> u32 {
    let o = off as usize;
    b.get(o..o + 4).map_or(0, |s| u32::from_le_bytes(s.try_into().unwrap()))
}
pub fn rd_i32(b: &[u8], off: u64) -> i32 {
    rd_u32(b, off) as i32
}
pub fn rd_u8(b: &[u8], off: u64) -> u8 {
    b.get(off as usize).copied().unwrap_or(0)
}

/// One line of /proc/PID/maps.
pub struct Mapping {
    pub start: u64,
    pub end: u64,
    pub offset: u64,
    pub path: String,
}

pub fn read_maps(pid: i32) -> MemResult<Vec<Mapping>> {
    let text = std::fs::read_to_string(format!("/proc/{}/maps", pid)).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => MemError::NoProcess,
        io::ErrorKind::PermissionDenied => MemError::Permission(format!("cannot read /proc/{}/maps", pid)),
        _ => MemError::Os(e),
    })?;
    let mut out = Vec::new();
    for line in text.lines() {
        let mut it = line.splitn(6, ' ');
        let range = it.next().unwrap_or("");
        let _perms = it.next();
        let offset = it.next().unwrap_or("0");
        let _dev = it.next();
        let _inode = it.next();
        let path = it.next().unwrap_or("").trim_start().to_string();
        let Some((a, b)) = range.split_once('-') else { continue };
        let (Ok(start), Ok(end), Ok(offset)) = (u64::from_str_radix(a, 16), u64::from_str_radix(b, 16), u64::from_str_radix(offset, 16)) else {
            continue;
        };
        out.push(Mapping { start, end, offset, path });
    }
    Ok(out)
}

/// What the ELF headers of the executable say about the debug section.
struct ElfInfo {
    is_pie: bool,
    /// virtual address of the section
    section_addr: u64,
    section_size: u64,
    /// lowest p_vaddr of a PT_LOAD segment with file offset 0
    first_load_vaddr: u64,
}

fn parse_elf(data: &[u8], section: &str) -> Option<ElfInfo> {
    if data.len() < 64 || &data[0..4] != b"\x7fELF" || data[4] != 2 || data[5] != 1 {
        return None; // 64-bit little-endian only
    }
    let e_type = u16::from_le_bytes([data[16], data[17]]);
    let e_phoff = u64::from_le_bytes(data[32..40].try_into().ok()?);
    let e_shoff = u64::from_le_bytes(data[40..48].try_into().ok()?);
    let e_phentsize = u16::from_le_bytes([data[54], data[55]]) as u64;
    let e_phnum = u16::from_le_bytes([data[56], data[57]]) as u64;
    let e_shentsize = u16::from_le_bytes([data[58], data[59]]) as u64;
    let e_shnum = u16::from_le_bytes([data[60], data[61]]) as u64;
    let e_shstrndx = u16::from_le_bytes([data[62], data[63]]) as u64;
    let mut first_load_vaddr = u64::MAX;
    for i in 0..e_phnum {
        let o = (e_phoff + i * e_phentsize) as usize;
        let ph = data.get(o..o + 56)?;
        let p_type = u32::from_le_bytes(ph[0..4].try_into().ok()?);
        let p_offset = u64::from_le_bytes(ph[8..16].try_into().ok()?);
        let p_vaddr = u64::from_le_bytes(ph[16..24].try_into().ok()?);
        if p_type == 1 && p_offset == 0 {
            first_load_vaddr = first_load_vaddr.min(p_vaddr);
        }
    }
    if first_load_vaddr == u64::MAX {
        first_load_vaddr = 0;
    }
    let sh = |i: u64| -> Option<&[u8]> {
        let o = (e_shoff + i * e_shentsize) as usize;
        data.get(o..o + 64)
    };
    let strtab = sh(e_shstrndx)?;
    let str_off = u64::from_le_bytes(strtab[24..32].try_into().ok()?) as usize;
    let str_size = u64::from_le_bytes(strtab[32..40].try_into().ok()?) as usize;
    let names = data.get(str_off..str_off + str_size)?;
    for i in 0..e_shnum {
        let s = sh(i)?;
        let name_off = u32::from_le_bytes(s[0..4].try_into().ok()?) as usize;
        let name = names.get(name_off..)?;
        let end = name.iter().position(|&b| b == 0)?;
        if &name[..end] == section.as_bytes() {
            return Some(ElfInfo {
                is_pie: e_type == 3,
                section_addr: u64::from_le_bytes(s[16..24].try_into().ok()?),
                section_size: u64::from_le_bytes(s[32..40].try_into().ok()?),
                first_load_vaddr,
            });
        }
    }
    None
}

/// Read only what the headers need: the ELF header, program headers and section
/// headers plus the section-name table (the executable is large).
fn read_elf_headers(path: &str) -> io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut head = vec![0u8; 64];
    f.read_exact(&mut head)?;
    if &head[0..4] != b"\x7fELF" {
        return Ok(head);
    }
    let e_shoff = u64::from_le_bytes(head[40..48].try_into().unwrap());
    let e_shentsize = u16::from_le_bytes([head[58], head[59]]) as u64;
    let e_shnum = u16::from_le_bytes([head[60], head[61]]) as u64;
    let e_shstrndx = u16::from_le_bytes([head[62], head[63]]) as u64;
    // Build a sparse image: header area + section headers + the name table, at their
    // file offsets (zero-filled elsewhere is fine for parse_elf).
    let sh_end = e_shoff + e_shnum * e_shentsize;
    if sh_end > len || e_shnum == 0 {
        return Ok(head);
    }
    let mut img = vec![0u8; sh_end as usize];
    f.seek(SeekFrom::Start(0))?;
    let first = 64 * 1024usize.min(img.len());
    let first = first.min(img.len());
    f.read_exact(&mut img[..first])?;
    f.seek(SeekFrom::Start(e_shoff))?;
    f.read_exact(&mut img[e_shoff as usize..sh_end as usize])?;
    let so = (e_shoff + e_shstrndx * e_shentsize) as usize;
    let str_off = u64::from_le_bytes(img[so + 24..so + 32].try_into().unwrap()) as usize;
    let str_size = u64::from_le_bytes(img[so + 32..so + 40].try_into().unwrap()) as usize;
    if str_off + str_size <= img.len() {
        f.seek(SeekFrom::Start(str_off as u64))?;
        let mut tmp = vec![0u8; str_size];
        f.read_exact(&mut tmp)?;
        img[str_off..str_off + str_size].copy_from_slice(&tmp);
    }
    Ok(img)
}

/// Address of `section` (and its size) in the running process `pid`, found through the
/// headers of its executable and the load bias from its memory map.
pub fn find_section(pid: i32, section: &str) -> MemResult<Option<(u64, u64)>> {
    let exe = format!("/proc/{}/exe", pid);
    let target = std::fs::read_link(&exe).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => MemError::NoProcess,
        io::ErrorKind::PermissionDenied => MemError::Permission(format!("cannot read {}", exe)),
        _ => MemError::Os(e),
    })?;
    let data = match read_elf_headers(&exe) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Err(MemError::Permission(format!("cannot open {}", exe))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(MemError::NoProcess),
        Err(e) => return Err(MemError::Os(e)),
    };
    let Some(info) = parse_elf(&data, section) else { return Ok(None) };
    if !info.is_pie {
        return Ok(Some((info.section_addr, info.section_size)));
    }
    let mut path = target.to_string_lossy().into_owned();
    let maps = read_maps(pid)?;
    // a deleted executable shows up as "<path> (deleted)"
    let strip = |p: &str| p.strip_suffix(" (deleted)").unwrap_or(p).to_string();
    path = strip(&path);
    let base = maps.iter().filter(|m| strip(&m.path) == path && m.offset == 0).map(|m| m.start).min();
    match base {
        Some(b) => Ok(Some((b - info.first_load_vaddr + info.section_addr, info.section_size))),
        None => Ok(None),
    }
}

/// Scheduler state letter of a thread (`R` running, `S` sleeping, ...).
pub fn thread_state(pid: i32, tid: u64) -> Option<char> {
    let s = std::fs::read_to_string(format!("/proc/{}/task/{}/stat", pid, tid)).ok()?;
    let i = s.rfind(')')?;
    s[i + 1..].trim_start().chars().next()
}

/// Children of `pid` (optionally all descendants), from /proc.
pub fn child_pids(pid: i32, recursive: bool) -> Vec<i32> {
    let mut parents: Vec<(i32, i32)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Ok(p) = e.file_name().to_string_lossy().parse::<i32>() else { continue };
            let Ok(s) = std::fs::read_to_string(format!("/proc/{}/stat", p)) else { continue };
            let Some(i) = s.rfind(')') else { continue };
            let mut f = s[i + 1..].split_whitespace();
            let _state = f.next();
            if let Some(Ok(pp)) = f.next().map(|x| x.parse::<i32>()) {
                parents.push((p, pp));
            }
        }
    }
    let mut out = Vec::new();
    let mut frontier = vec![pid];
    while let Some(cur) = frontier.pop() {
        for &(p, pp) in &parents {
            if pp == cur && !out.contains(&p) {
                out.push(p);
                if recursive {
                    frontier.push(p);
                }
            }
        }
    }
    out
}
