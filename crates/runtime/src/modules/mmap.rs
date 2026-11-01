//! The `mmap` module (Linux): memory-mapped files with the buffer protocol.

use crate::abstract_ as ab;
use crate::args::Params;
use crate::buffer;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, Trav};
use crate::object::*;
use crate::types::typeobject::tname;
use crate::types::{bool_, bytes, dict, exceptions, int, none, slice, str as pystr, typeobject};
use crate::{mdefs, raise};
use std::ffi::{c_int, c_void};

const ACCESS_DEFAULT: i32 = 0;
const ACCESS_READ: i32 = 1;
const ACCESS_WRITE: i32 = 2;
const ACCESS_COPY: i32 = 3;

pub struct Mmap {
    data: *mut u8,
    size: usize,
    pos: usize,
    offset: i64,
    /// dup'd file descriptor (-1 for anonymous maps or trackfd=False)
    fd: i32,
    flags: i32,
    prot: i32,
    access: i32,
    trackfd: bool,
    exports: isize,
}

impl Trav for Mmap {}

impl Drop for Mmap {
    fn drop(&mut self) {
        unsafe {
            if !self.data.is_null() {
                libc::munmap(self.data as *mut c_void, self.size.max(1));
                self.data = std::ptr::null_mut();
            }
            if self.fd >= 0 {
                libc::close(self.fd);
                self.fd = -1;
            }
        }
    }
}

#[unsafe(no_mangle)]
pub static mut RPY_MMAP_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn mmap_type() -> *mut PyTypeObject {
    &raw mut RPY_MMAP_TYPE
}

#[inline]
fn st(o: &PyObject) -> &mut Mmap {
    fc::rs::<Mmap>(o)
}

fn check_valid(o: &PyObject) -> PyResult<&mut Mmap> {
    let m = st(o);
    if m.data.is_null() {
        raise!(ValueError, "mmap closed or invalid");
    }
    Ok(m)
}

fn check_writable(m: &Mmap) -> PyResult<()> {
    if m.access == ACCESS_READ || m.prot & libc::PROT_WRITE == 0 {
        raise!(TypeError, "mmap can't modify a readonly memory map.");
    }
    Ok(())
}

fn data(m: &Mmap) -> &mut [u8] {
    if m.data.is_null() || m.size == 0 {
        return &mut [];
    }
    unsafe { std::slice::from_raw_parts_mut(m.data, m.size) }
}

fn ssize_arg(o: &PyObject) -> PyResult<isize> {
    if crate::types::float::is_float(o) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    if !ab::has_index(o) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(o));
    }
    ab::index_as_ssize(o, Some(exceptions::OverflowError()))
}

fn os_err() -> PyErr {
    crate::modules::posix::path::os_err()
}

// ------------------------------------------------------------------ construction

unsafe extern "C" fn mmap_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    let a = unsafe { fc::VcArgs::from_c(args, kw) };
    into_c(mmap_new_impl(tp, &a))
}

fn mmap_new_impl(tp: *mut PyTypeObject, a: &fc::VcArgs) -> PyResult {
    static P: Params = Params::new("mmap", &["fileno", "length", "flags", "prot", "access", "offset", "trackfd"], 0, 2).kwonly(1);
    let [fileno, length, flags, prot, access, offset, trackfd] = a.parse::<7>(&P)?;
    let fd = crate::modules::posix::path::c_int(fileno.as_deref().unwrap())?;
    let map_size = ssize_arg(length.as_deref().unwrap())?;
    let mut flags = match flags {
        Some(f) => crate::modules::posix::path::c_int(&f)?,
        None => libc::MAP_SHARED,
    };
    let mut prot = match prot {
        Some(p) => crate::modules::posix::path::c_int(&p)?,
        None => libc::PROT_WRITE | libc::PROT_READ,
    };
    let mut access = match access {
        Some(x) => crate::modules::posix::path::c_int(&x)?,
        None => ACCESS_DEFAULT,
    };
    let offset: i64 = match offset {
        Some(o) => {
            if o.is(none::none()) {
                raise!(TypeError, "'NoneType' object cannot be interpreted as an integer");
            }
            crate::modules::posix::path::c_long(&o)?
        }
        None => 0,
    };
    let trackfd = match trackfd {
        Some(t) => ab::is_true(&t)?,
        None => true,
    };
    if map_size < 0 {
        raise!(OverflowError, "memory mapped length must be positive");
    }
    if offset < 0 {
        raise!(OverflowError, "memory mapped offset must be positive");
    }
    if access != ACCESS_DEFAULT && (flags != libc::MAP_SHARED || prot != (libc::PROT_WRITE | libc::PROT_READ)) {
        raise!(ValueError, "mmap can't specify both access and flags, prot.");
    }
    match access {
        ACCESS_READ => {
            flags = libc::MAP_SHARED;
            prot = libc::PROT_READ;
        }
        ACCESS_WRITE => {
            flags = libc::MAP_SHARED;
            prot = libc::PROT_READ | libc::PROT_WRITE;
        }
        ACCESS_COPY => {
            flags = libc::MAP_PRIVATE;
            prot = libc::PROT_READ | libc::PROT_WRITE;
        }
        ACCESS_DEFAULT => {
            if prot & libc::PROT_READ != 0 && prot & libc::PROT_WRITE != 0 {
                // default
            } else if prot & libc::PROT_WRITE != 0 {
                access = ACCESS_WRITE;
            } else {
                access = ACCESS_READ;
            }
        }
        _ => raise!(ValueError, "mmap invalid access parameter."),
    }
    crate::modules::sys_full::audit(
        "mmap.__new__",
        &[&int::new_int(fd as i64), &int::new_int(map_size as i64), &int::new_int(access as i64), &int::new_int(offset)],
    )?;
    let mut map_size = map_size as usize;
    if fd != -1 {
        let mut stbuf: libc::stat = unsafe { std::mem::zeroed() };
        let ok = crate::threads::blocking(|| unsafe { libc::fstat(fd, &mut stbuf) }) == 0;
        if ok && (stbuf.st_mode & libc::S_IFMT) == libc::S_IFREG {
            let fsize = stbuf.st_size as i64;
            if map_size == 0 {
                if fsize == 0 {
                    raise!(ValueError, "cannot mmap an empty file");
                }
                if offset >= fsize {
                    raise!(ValueError, "mmap offset is greater than file size");
                }
                if fsize - offset > isize::MAX as i64 {
                    raise!(ValueError, "mmap length is too large");
                }
                map_size = (fsize - offset) as usize;
            } else if offset > fsize || fsize - offset < map_size as i64 {
                raise!(ValueError, "mmap length is greater than file size");
            }
        }
    }
    let mut myfd = -1;
    if fd == -1 {
        flags |= libc::MAP_ANONYMOUS;
    } else if trackfd {
        myfd = unsafe { libc::dup(fd) };
        if myfd == -1 {
            return Err(os_err());
        }
    }
    let p = crate::threads::blocking(|| unsafe {
        libc::mmap(std::ptr::null_mut(), map_size, prot, flags, fd, offset as libc::off_t)
    });
    if p == libc::MAP_FAILED {
        let e = os_err();
        if myfd >= 0 {
            unsafe { libc::close(myfd) };
        }
        return Err(e);
    }
    fc::robj_new(
        tp,
        Mmap { data: p as *mut u8, size: map_size, pos: 0, offset, fd: myfd, flags, prot, access, trackfd, exports: 0 },
    )
}

// ------------------------------------------------------------------ methods

fn m_close(o: &PyObject) -> PyResult {
    let m = st(o);
    if m.exports > 0 {
        raise!(BufferError, "cannot close exported pointers exist");
    }
    unsafe {
        if m.fd >= 0 {
            libc::close(m.fd);
            m.fd = -1;
        }
        if !m.data.is_null() {
            libc::munmap(m.data as *mut c_void, m.size.max(1));
            m.data = std::ptr::null_mut();
        }
    }
    Ok(none::none_ref())
}

fn m_closed(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(st(o).data.is_null()))
}

fn m_enter(o: &PyObject) -> PyResult {
    check_valid(o)?;
    Ok(o.to_ref())
}

fn m_exit(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    m_close(o)
}

/// bytes.find-style index normalisation.
fn adjust(start: isize, end: isize, len: isize) -> (isize, isize) {
    let mut s = start;
    let mut e = end;
    if e > len {
        e = len;
    } else if e < 0 {
        e += len;
        if e < 0 {
            e = 0;
        }
    }
    if s < 0 {
        s += len;
        if s < 0 {
            s = 0;
        }
    }
    (s, e)
}

fn gfind(o: &PyObject, args: &[&PyObject], reverse: bool) -> PyResult {
    let fname = if reverse { "rfind" } else { "find" };
    crate::args::check_pos(fname, args, 1, 3)?;
    check_valid(o)?;
    let sub = buffer::with_buffer(args[0], |b| b.to_vec())?;
    let start = match args.get(1) {
        Some(s) if !s.is(none::none()) => Some(ssize_arg(s)?),
        _ => None,
    };
    let end = match args.get(2) {
        Some(e) if !e.is(none::none()) => Some(ssize_arg(e)?),
        _ => None,
    };
    let m = check_valid(o)?;
    let len = m.size as isize;
    let (s, e) = adjust(start.unwrap_or(m.pos as isize), end.unwrap_or(len), len);
    if s > len || e - s < sub.len() as isize {
        return Ok(int::new_int(-1));
    }
    let hay = &data(m)[s as usize..e as usize];
    let r = if reverse {
        if sub.is_empty() {
            Some(hay.len())
        } else {
            hay.windows(sub.len()).rposition(|w| w == &sub[..])
        }
    } else if sub.is_empty() {
        Some(0)
    } else {
        memfind(hay, &sub)
    };
    Ok(int::new_int(match r {
        Some(i) => s as i64 + i as i64,
        None => -1,
    }))
}

fn memfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    let first = needle[0];
    let last = hay.len() - needle.len();
    let mut i = 0;
    while i <= last {
        match hay[i..=last].iter().position(|&b| b == first) {
            None => return None,
            Some(k) => {
                i += k;
                if &hay[i..i + needle.len()] == needle {
                    return Some(i);
                }
                i += 1;
            }
        }
    }
    None
}

fn m_find(o: &PyObject, args: &[&PyObject]) -> PyResult {
    gfind(o, args, false)
}
fn m_rfind(o: &PyObject, args: &[&PyObject]) -> PyResult {
    gfind(o, args, true)
}

fn m_flush(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("flush", &["offset", "size", "flags"], 2, 0).kwonly(1);
    let [off, size, flags] = crate::args::parse::<3>(&P, args, kw)?;
    let off = match off {
        Some(x) => ssize_arg(x)?,
        None => 0,
    };
    let size = match size {
        Some(x) => Some(ssize_arg(x)?),
        None => None,
    };
    let flags = match flags {
        Some(x) => crate::modules::posix::path::c_int(x)?,
        None => libc::MS_SYNC,
    };
    let m = check_valid(o)?;
    let size = size.unwrap_or(m.size as isize - off);
    if size < 0 || off < 0 || (m.size as isize) - off < size {
        raise!(ValueError, "flush values out of range");
    }
    if m.access == ACCESS_READ || m.access == ACCESS_COPY {
        return Ok(none::none_ref());
    }
    let p = unsafe { m.data.add(off as usize) };
    if crate::threads::blocking(|| unsafe { libc::msync(p as *mut c_void, size as usize, flags) }) == -1 {
        return Err(os_err());
    }
    Ok(none::none_ref())
}

fn m_madvise(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("madvise", args, 1, 3)?;
    let option = crate::modules::posix::path::c_int(args[0])?;
    let start = match args.get(1) {
        Some(s) => ssize_arg(s)?,
        None => 0,
    };
    let length = match args.get(2) {
        Some(l) => Some(ssize_arg(l)?),
        None => None,
    };
    let m = check_valid(o)?;
    let size = m.size as isize;
    let mut length = length.unwrap_or(size);
    if start < 0 || start >= size {
        raise!(ValueError, "madvise start out of bounds");
    }
    if length < 0 {
        raise!(ValueError, "madvise length invalid");
    }
    if isize::MAX - start < length {
        raise!(OverflowError, "madvise length too large");
    }
    if start + length > size {
        length = size - start;
    }
    let p = unsafe { m.data.add(start as usize) };
    if unsafe { libc::madvise(p as *mut c_void, length as usize, option) } != 0 {
        return Err(os_err());
    }
    Ok(none::none_ref())
}

fn m_move(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("move", args, 3, 3)?;
    let dest = ssize_arg(args[0])?;
    let src = ssize_arg(args[1])?;
    let cnt = ssize_arg(args[2])?;
    let m = check_valid(o)?;
    check_writable(m)?;
    let size = m.size as isize;
    if dest < 0 || src < 0 || cnt < 0 || size - dest < cnt || size - src < cnt {
        raise!(ValueError, "source, destination, or count out of range");
    }
    unsafe { std::ptr::copy(m.data.add(src as usize), m.data.add(dest as usize), cnt as usize) };
    Ok(none::none_ref())
}

fn m_read(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("read", args, 0, 1)?;
    let n = match args.first() {
        Some(a) if !a.is(none::none()) => {
            if !ab::has_index(a) || crate::types::float::is_float(a) {
                raise!(TypeError, "argument should be integer or None, not '{}'", tname(a));
            }
            Some(ab::index_as_ssize(a, Some(exceptions::OverflowError()))?)
        }
        _ => None,
    };
    let m = check_valid(o)?;
    let remaining = if m.pos < m.size { m.size - m.pos } else { 0 };
    let n = match n {
        Some(n) if n >= 0 && (n as usize) <= remaining => n as usize,
        _ => remaining,
    };
    let start = m.pos.min(m.size);
    let r = bytes::new_bytes(&data(m)[start..start + n]);
    m.pos += n;
    Ok(r)
}

fn m_read_byte(o: &PyObject) -> PyResult {
    let m = check_valid(o)?;
    if m.pos >= m.size {
        raise!(ValueError, "read byte out of range");
    }
    let b = data(m)[m.pos];
    m.pos += 1;
    Ok(int::new_int(b as i64))
}

fn m_readline(o: &PyObject) -> PyResult {
    let m = check_valid(o)?;
    let remaining = if m.pos < m.size { m.size - m.pos } else { 0 };
    let start = m.pos.min(m.size);
    let d = &data(m)[start..start + remaining];
    let n = match d.iter().position(|&b| b == b'\n') {
        Some(i) => i + 1,
        None => remaining,
    };
    let r = bytes::new_bytes(&d[..n]);
    m.pos += n;
    Ok(r)
}

fn m_resize(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("resize", args, 1, 1)?;
    let new_size = ssize_arg(args[0])?;
    let m = check_valid(o)?;
    if m.exports > 0 {
        raise!(BufferError, "mmap can't resize with extant buffers exported.");
    }
    if m.access != ACCESS_WRITE && m.access != ACCESS_DEFAULT {
        raise!(TypeError, "mmap can't resize a readonly or copy-on-write memory map.");
    }
    if !m.trackfd {
        raise!(ValueError, "mmap can't resize with trackfd=False.");
    }
    if new_size < 0 || ((isize::MAX - new_size) as i64) < m.offset {
        raise!(ValueError, "new size out of range");
    }
    let new_size = new_size as usize;
    if m.fd == -1 && m.flags & libc::MAP_PRIVATE == 0 && new_size > m.size {
        raise!(ValueError, "mmap: can't expand a shared anonymous mapping on Linux");
    }
    if m.fd != -1 && unsafe { libc::ftruncate(m.fd, (m.offset + new_size as i64) as libc::off_t) } == -1 {
        return Err(os_err());
    }
    let p = unsafe { libc::mremap(m.data as *mut c_void, m.size.max(1), new_size.max(1), libc::MREMAP_MAYMOVE) };
    if p == libc::MAP_FAILED {
        return Err(os_err());
    }
    m.data = p as *mut u8;
    m.size = new_size;
    Ok(none::none_ref())
}

fn m_seek(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("seek", args, 1, 2)?;
    let dist = ssize_arg(args[0])?;
    let how = match args.get(1) {
        Some(h) => crate::modules::posix::path::c_int(h)?,
        None => 0,
    };
    let m = check_valid(o)?;
    let base: isize = match how {
        0 => 0,
        1 => m.pos as isize,
        2 => m.size as isize,
        _ => raise!(ValueError, "unknown seek type"),
    };
    let where_ = match base.checked_add(dist) {
        Some(w) => w,
        None => raise!(ValueError, "seek out of range"),
    };
    if where_ > m.size as isize || where_ < 0 {
        raise!(ValueError, "seek out of range");
    }
    m.pos = where_ as usize;
    Ok(int::new_int(where_ as i64))
}

fn m_seekable(_o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(true))
}

fn m_size(o: &PyObject) -> PyResult {
    let m = check_valid(o)?;
    if m.fd != -1 {
        let mut stbuf: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(m.fd, &mut stbuf) } == -1 {
            return Err(os_err());
        }
        return Ok(int::new_int(stbuf.st_size as i64));
    }
    if m.trackfd {
        return Ok(int::new_int(m.size as i64));
    }
    raise!(ValueError, "mmap can't get size with trackfd=False.")
}

fn m_tell(o: &PyObject) -> PyResult {
    let m = check_valid(o)?;
    Ok(int::new_int(m.pos as i64))
}

fn m_write(o: &PyObject, arg: &PyObject) -> PyResult {
    check_valid(o)?;
    let d = buffer::with_buffer(arg, |b| b.to_vec())?;
    let m = check_valid(o)?;
    check_writable(m)?;
    if m.pos > m.size || m.size - m.pos < d.len() {
        raise!(ValueError, "data out of range");
    }
    data(m)[m.pos..m.pos + d.len()].copy_from_slice(&d);
    m.pos += d.len();
    Ok(int::new_int(d.len() as i64))
}

fn byte_value(v: &PyObject) -> PyResult<u8> {
    if crate::types::float::is_float(v) || !ab::has_index(v) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(v));
    }
    let x = ab::as_i64(&*ab::index(v)?)?;
    if x < 0 {
        raise!(OverflowError, "unsigned byte integer is less than minimum");
    }
    if x > 255 {
        raise!(OverflowError, "unsigned byte integer is greater than maximum");
    }
    Ok(x as u8)
}

fn m_write_byte(o: &PyObject, arg: &PyObject) -> PyResult {
    check_valid(o)?;
    let b = byte_value(arg)?;
    let m = check_valid(o)?;
    check_writable(m)?;
    if m.pos >= m.size {
        raise!(ValueError, "write byte out of range");
    }
    data(m)[m.pos] = b;
    m.pos += 1;
    Ok(none::none_ref())
}

fn m_set_name(o: &PyObject, arg: &PyObject) -> PyResult {
    if !pystr::is_str(arg) {
        raise!(TypeError, "set_name() argument must be str, not {}", tname(arg));
    }
    let name = pystr::as_utf8(arg)?.to_string();
    let m = check_valid(o)?;
    if m.flags & libc::MAP_ANONYMOUS == 0 {
        raise!(ValueError, "mmap: set_name() is only supported for anonymous memory mappings");
    }
    let full = format!("cpython:mmap:{}", name);
    if full.len() + 1 > 80 {
        raise!(ValueError, "mmap: name is too long");
    }
    if full.bytes().any(|b| b == 0) {
        raise!(ValueError, "embedded null character");
    }
    let c = std::ffi::CString::new(full).unwrap();
    const PR_SET_VMA: c_int = 0x53564d41;
    const PR_SET_VMA_ANON_NAME: libc::c_ulong = 0;
    let r = unsafe {
        libc::prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME, m.data as libc::c_ulong, m.size as libc::c_ulong, c.as_ptr() as libc::c_ulong)
    };
    if r != 0 {
        return Err(os_err());
    }
    Ok(none::none_ref())
}

fn m_sizeof(o: &PyObject) -> PyResult {
    let _ = o;
    Ok(int::new_int(std::mem::size_of::<fc::RObj<Mmap>>() as i64))
}

// ------------------------------------------------------------------ slots

unsafe extern "C" fn mmap_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let ob = &*o;
        let m = st(ob);
        let tpname = std::ffi::CStr::from_ptr((*ob.type_ptr()).tp_name).to_string_lossy().into_owned();
        if m.data.is_null() {
            return pystr::new_str(&format!("<{} closed=True>", tpname)).into_raw();
        }
        let access = match m.access {
            ACCESS_READ => "ACCESS_READ",
            ACCESS_WRITE => "ACCESS_WRITE",
            ACCESS_COPY => "ACCESS_COPY",
            _ => "ACCESS_DEFAULT",
        };
        pystr::new_str(&format!(
            "<{} closed=False, access={}, length={}, pos={}, offset={}>",
            tpname, access, m.size, m.pos, m.offset
        ))
        .into_raw()
    }
}

unsafe extern "C" fn mmap_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        match check_valid(&*o) {
            Ok(m) => m.size as Py_ssize_t,
            Err(_) => -1,
        }
    }
}

unsafe extern "C" fn mmap_item(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let m = check_valid(&*o)?;
            if i < 0 || i as usize >= m.size {
                raise!(IndexError, "mmap index out of range");
            }
            Ok(bytes::new_bytes(&data(m)[i as usize..i as usize + 1]))
        })())
    }
}

fn subscript(o: &PyObject, item: &PyObject) -> PyResult {
    check_valid(o)?;
    if slice::is_slice(item) {
        let len = st(o).size as isize;
        let (start, _stop, step, n) = slice::indices(item, len)?;
        let m = check_valid(o)?;
        let d = data(m);
        let mut out = Vec::with_capacity(n.max(0) as usize);
        let mut i = start;
        for _ in 0..n {
            if i < 0 || i as usize >= d.len() {
                raise!(IndexError, "mmap index out of range");
            }
            out.push(d[i as usize]);
            i = i.wrapping_add(step);
        }
        return Ok(bytes::new_bytes(&out));
    }
    if !ab::has_index(item) {
        raise!(TypeError, "mmap indices must be integers");
    }
    let mut i = ab::index_as_ssize(item, Some(exceptions::IndexError()))?;
    let m = check_valid(o)?;
    if i < 0 {
        i += m.size as isize;
    }
    if i < 0 || i as usize >= m.size {
        raise!(IndexError, "mmap index out of range");
    }
    Ok(int::new_int(data(m)[i as usize] as i64))
}

unsafe extern "C" fn mmap_subscript(o: *mut PyObject, item: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(subscript(&*o, &*item)) }
}

fn ass_subscript(o: &PyObject, item: &PyObject, value: Option<&PyObject>) -> PyResult<()> {
    {
        let m = check_valid(o)?;
        check_writable(m)?;
    }
    let value = match value {
        Some(v) => v,
        None => raise!(TypeError, "mmap object doesn't support item deletion"),
    };
    if slice::is_slice(item) {
        let len = st(o).size as isize;
        let (start, _stop, step, n) = slice::indices(item, len)?;
        check_valid(o)?;
        let vb = buffer::with_buffer(value, |b| b.to_vec())?;
        if vb.len() as isize != n {
            raise!(IndexError, "mmap slice assignment is wrong size");
        }
        let m = check_valid(o)?;
        let d = data(m);
        // the buffer callback may have resized the map: re-check bounds
        let mut i = start;
        for _ in 0..n {
            if i < 0 || i as usize >= d.len() {
                raise!(IndexError, "mmap index out of range");
            }
            i = i.wrapping_add(step);
        }
        let mut i = start;
        for b in vb {
            d[i as usize] = b;
            i = i.wrapping_add(step);
        }
        return Ok(());
    }
    if !ab::has_index(item) {
        raise!(TypeError, "mmap indices must be integer");
    }
    let mut i = ab::index_as_ssize(item, Some(exceptions::IndexError()))?;
    {
        let m = check_valid(o)?;
        if i < 0 {
            i += m.size as isize;
        }
        if i < 0 || i as usize >= m.size {
            raise!(IndexError, "mmap index out of range");
        }
    }
    if !ab::has_index(value) || crate::types::float::is_float(value) {
        raise!(TypeError, "mmap item value must be an int");
    }
    let v = ab::as_i64(&*ab::index(value)?)?;
    if !(0..256).contains(&v) {
        raise!(ValueError, "mmap item value must be in range(0, 256)");
    }
    let m = check_valid(o)?;
    if i as usize >= m.size {
        raise!(IndexError, "mmap index out of range");
    }
    data(m)[i as usize] = v as u8;
    Ok(())
}

unsafe extern "C" fn mmap_ass_subscript(o: *mut PyObject, item: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { int_into_c(ass_subscript(&*o, &*item, fc::opt(v))) }
}

unsafe extern "C" fn mmap_ass_item(o: *mut PyObject, i: Py_ssize_t, v: *mut PyObject) -> c_int {
    unsafe { int_into_c(ass_subscript(&*o, &int::new_int(i as i64), fc::opt(v))) }
}

unsafe extern "C" fn mmap_getbuffer(o: *mut PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    unsafe {
        let ob = &*o;
        let m = match check_valid(ob) {
            Ok(m) => m,
            Err(_) => return -1,
        };
        let readonly = m.access == ACCESS_READ || m.prot & libc::PROT_WRITE == 0;
        if buffer::fill_info(view, o, m.data as *mut c_void, m.size as isize, readonly, flags).is_err() {
            return -1;
        }
        m.exports += 1;
        0
    }
}

unsafe extern "C" fn mmap_releasebuffer(o: *mut PyObject, _view: *mut Py_buffer) {
    unsafe {
        st(&*o).exports -= 1;
    }
}

fn leak<T>(v: T) -> *mut T {
    Box::into_raw(Box::new(v))
}

fn init_types() -> PyResult<()> {
    unsafe {
        let t = mmap_type();
        if (*t).tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        fc::init_robj_type::<Mmap>(t, c"mmap.mmap", Py_TPFLAGS_BASETYPE);
        (*t).tp_new = Some(mmap_new);
        (*t).tp_repr = Some(mmap_repr);
        (*t).tp_setattro = Some(typeobject::object_generic_setattro);
        let mut sq: PySequenceMethods = std::mem::zeroed();
        sq.sq_length = Some(mmap_len);
        sq.sq_item = Some(mmap_item);
        sq.sq_ass_item = Some(mmap_ass_item);
        (*t).tp_as_sequence = leak(sq);
        let mut mp: PyMappingMethods = std::mem::zeroed();
        mp.mp_length = Some(mmap_len);
        mp.mp_subscript = Some(mmap_subscript);
        mp.mp_ass_subscript = Some(mmap_ass_subscript);
        (*t).tp_as_mapping = leak(mp);
        let mut bp: PyBufferProcs = std::mem::zeroed();
        bp.bf_getbuffer = Some(mmap_getbuffer);
        bp.bf_releasebuffer = Some(mmap_releasebuffer);
        (*t).tp_as_buffer = leak(bp);
        (*t).tp_doc = c"mmap(fileno, length, flags=MAP_SHARED, prot=PROT_WRITE|PROT_READ,\n     access=ACCESS_DEFAULT, offset=0, *, trackfd=True)\n\nMemory-map length bytes of the file open as fileno, starting at offset\n(a multiple of ALLOCATIONGRANULARITY). A length of 0 maps the rest of the\nfile. fileno -1 creates an anonymous mapping. Use either access\n(ACCESS_READ, ACCESS_WRITE, ACCESS_COPY) or flags and prot. The object\nsupports slicing, file-like reading and writing and the buffer protocol.".as_ptr();
        (*t).tp_methods = mdefs![
            ("close", NOARGS, m_close),
            ("find", FAST, m_find, "find($self, sub, start=None, end=None, /)\n--\n\nReturn the lowest index where sub is found in [start:end], or -1."),
            ("rfind", FAST, m_rfind, "rfind($self, sub, start=None, end=None, /)\n--\n\nReturn the highest index where sub is found in [start:end], or -1."),
            ("flush", FASTKW, m_flush, "flush($self, offset=0, size=None, /)\n--\n\nWrite changes in the given range back to the file."),
            ("madvise", FAST, m_madvise, "madvise($self, option, start=0, length=None, /)\n--\n\nGive the kernel advice about the use of a memory range."),
            ("move", FAST, m_move, "move($self, dest, src, count, /)\n--\n\nCopy count bytes from offset src to offset dest."),
            ("read", FAST, m_read, "read($self, n=None, /)\n--\n\nRead up to n bytes from the current position (all remaining if None)."),
            ("read_byte", NOARGS, m_read_byte),
            ("readline", NOARGS, m_readline),
            ("resize", FAST, m_resize, "resize($self, newsize, /)\n--\n\nChange the size of the mapping (and of the underlying file)."),
            ("seek", FAST, m_seek, "seek($self, pos, whence=0, /)\n--\n\nChange the current position."),
            ("seekable", NOARGS, m_seekable),
            ("set_name", O, m_set_name),
            ("size", NOARGS, m_size),
            ("tell", NOARGS, m_tell),
            ("write", O, m_write),
            ("write_byte", O, m_write_byte),
            ("__enter__", NOARGS, m_enter),
            ("__exit__", FAST, m_exit, "__exit__($self, /, *args)\n--\n\nClose the mapping when leaving a with block."),
            ("__sizeof__", NOARGS, m_sizeof),
        ];
        (*t).tp_getset = crate::getset_table![("closed", m_closed)];
        typeobject::type_ready(t)?;
    }
    Ok(())
}

pub fn init() -> PyResult {
    init_types()?;
    let (m, d) = fc::new_module("mmap", std::ptr::null_mut(), None)?;
    dict::set_item_str(&d, "mmap", typeobject::type_obj(mmap_type()))?;
    dict::set_item_str(&d, "error", typeobject::type_obj(exceptions::OSError()))?;
    let pagesize = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as i64;
    let consts: &[(&str, i64)] = &[
        ("PAGESIZE", pagesize),
        ("ALLOCATIONGRANULARITY", pagesize),
        ("ACCESS_DEFAULT", 0),
        ("ACCESS_READ", 1),
        ("ACCESS_WRITE", 2),
        ("ACCESS_COPY", 3),
        ("PROT_EXEC", 4),
        ("PROT_READ", 1),
        ("PROT_WRITE", 2),
        ("MAP_SHARED", 1),
        ("MAP_PRIVATE", 2),
        ("MAP_DENYWRITE", 0x800),
        ("MAP_EXECUTABLE", 0x1000),
        ("MAP_ANON", 0x20),
        ("MAP_ANONYMOUS", 0x20),
        ("MAP_NORESERVE", 0x4000),
        ("MAP_POPULATE", 0x8000),
        ("MAP_STACK", 0x20000),
        ("MADV_NORMAL", 0),
        ("MADV_RANDOM", 1),
        ("MADV_SEQUENTIAL", 2),
        ("MADV_WILLNEED", 3),
        ("MADV_DONTNEED", 4),
        ("MADV_FREE", 8),
        ("MADV_REMOVE", 9),
        ("MADV_DONTFORK", 10),
        ("MADV_DOFORK", 11),
        ("MADV_MERGEABLE", 12),
        ("MADV_UNMERGEABLE", 13),
        ("MADV_HUGEPAGE", 14),
        ("MADV_NOHUGEPAGE", 15),
        ("MADV_DONTDUMP", 16),
        ("MADV_DODUMP", 17),
        ("MADV_HWPOISON", 100),
        ("MS_ASYNC", 1),
        ("MS_INVALIDATE", 2),
        ("MS_SYNC", 4),
    ];
    for (n, v) in consts {
        dict::set_item_str(&d, n, &int::new_int(*v))?;
    }
    Ok(m)
}
