//! BufferedReader, BufferedWriter, BufferedRandom, BufferedRWPair.
use super::fileio;
use super::*;
use crate::modules::bufview::BufView;
use crate::types::bytes;
use std::ffi::{c_int, c_void};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Reader,
    Writer,
    Random,
}

pub struct State {
    pub buffer_size: usize,
    /// read-ahead data; unread part is rbuf[rpos..]
    pub rbuf: Vec<u8>,
    pub rpos: usize,
    /// pending writes (logically located at the raw position)
    pub wbuf: Vec<u8>,
    pub readable: bool,
    pub writable: bool,
}

impl State {
    fn unread(&self) -> usize {
        self.rbuf.len() - self.rpos
    }
    fn reset_read(&mut self) {
        self.rbuf.clear();
        self.rpos = 0;
    }
}

#[repr(C)]
pub struct Buffered {
    pub hdr: IoHeader,
    pub raw: *mut PyObject,
    pub st: *mut State,
    pub detached: bool,
    pub kind: u8,
    pub finalizing: bool,
}

pub static mut BUFREADER_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut BUFWRITER_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut BUFRANDOM_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut BUFRWPAIR_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub fn bufreader_type() -> *mut PyTypeObject {
    &raw mut BUFREADER_TYPE
}
pub fn bufwriter_type() -> *mut PyTypeObject {
    &raw mut BUFWRITER_TYPE
}
pub fn bufrandom_type() -> *mut PyTypeObject {
    &raw mut BUFRANDOM_TYPE
}
pub fn bufrwpair_type() -> *mut PyTypeObject {
    &raw mut BUFRWPAIR_TYPE
}

#[inline]
fn bf(o: &PyObject) -> &mut Buffered {
    unsafe { &mut *(o.as_ptr() as *mut Buffered) }
}

pub fn is_buffered(o: &PyObject) -> bool {
    let t = o.type_ptr();
    crate::types::typeobject::is_subtype(t, bufreader_type())
        || crate::types::typeobject::is_subtype(t, bufwriter_type())
        || crate::types::typeobject::is_subtype(t, bufrandom_type())
}

/// (raw, state) of an initialized, attached object.
fn parts(o: &PyObject) -> PyResult<(PyRef, &mut State)> {
    let b = bf(o);
    if b.st.is_null() || b.raw.is_null() {
        if b.detached {
            raise!(ValueError, "raw stream has been detached");
        }
        raise!(ValueError, "I/O operation on uninitialized object");
    }
    Ok((unsafe { PyRef::from_borrowed(b.raw) }, unsafe { &mut *b.st }))
}

pub fn raw_of(o: &PyObject) -> PyResult<PyRef> {
    Ok(parts(o)?.0)
}

fn raw_closed(raw: &PyObject) -> PyResult<bool> {
    if fileio::is_exact_fileio(raw) {
        return Ok(fileio::fio(raw).fd < 0);
    }
    let c = ab::getattr(raw, crate::name!("closed"))?;
    ab::is_true(&c)
}

fn check_closed_msg(raw: &PyObject, msg: &str) -> PyResult<()> {
    if raw_closed(raw)? {
        raise!(ValueError, "{}", msg);
    }
    Ok(())
}

// ---------------------------------------------------------------- raw helpers

fn retry_interrupted<T>(mut f: impl FnMut() -> PyResult<T>) -> PyResult<T> {
    loop {
        match f() {
            Err(e) => {
                if exc_matches(crate::types::exceptions::InterruptedError()) {
                    clear_exc();
                    continue;
                }
                return Err(e);
            }
            ok => return ok,
        }
    }
}

/// raw.readinto(buf) -> Some(n) / None (would block).
fn raw_readinto(raw: &PyObject, buf: &mut [u8]) -> PyResult<Option<usize>> {
    if fileio::is_exact_fileio(raw) {
        let f = fileio::fio(raw);
        if f.fd >= 0 && f.readable {
            return fileio::raw_read_into(f.fd, buf);
        }
    }
    let len = buf.len();
    let ba = new_bytearray(len)?;
    let r = retry_interrupted(|| call1(raw, "readinto", &ba))?;
    if r.is(none::none()) {
        return Ok(None);
    }
    let n = match ab::index_as_ssize(&r, Some(crate::types::exceptions::OverflowError())) {
        Ok(n) => n,
        Err(_) => {
            let cause = take_exc();
            let _ = err!(OSError, "raw readinto() failed");
            if let (Some(c), Some(e)) = (cause, take_exc()) {
                crate::types::exceptions::set_exc_cause(&e, Some(&c));
                set_exc(e);
            }
            return Err(PyErr);
        }
    };
    if n < 0 || n as usize > len {
        raise!(OSError, "raw readinto() returned invalid length {} (should have been between 0 and {})", n, len);
    }
    let n = n as usize;
    let v = BufView::get(&ba, false)?;
    buf[..n].copy_from_slice(&v.as_slice()[..n]);
    Ok(Some(n))
}

fn raw_write(raw: &PyObject, data: &[u8]) -> PyResult<Option<usize>> {
    if fileio::is_exact_fileio(raw) {
        let f = fileio::fio(raw);
        if f.fd >= 0 && f.writable {
            return fileio::raw_write(f.fd, data);
        }
    }
    let b = new_bytes(data);
    let r = retry_interrupted(|| call1(raw, "write", &b))?;
    if r.is(none::none()) {
        return Ok(None);
    }
    let n = ab::index_as_ssize(&r, Some(crate::types::exceptions::OverflowError()))?;
    if n < 0 || n as usize > data.len() {
        raise!(OSError, "raw write() returned invalid length {} (should have been between 0 and {})", n, data.len());
    }
    Ok(Some(n as usize))
}

fn raw_tell(raw: &PyObject) -> PyResult<i64> {
    let r = if fileio::is_exact_fileio(raw) && fileio::fio(raw).fd >= 0 {
        let p = unsafe { libc::lseek(fileio::fio(raw).fd, 0, libc::SEEK_CUR) };
        if p < 0 {
            return Err(crate::modules::posix::path::os_err());
        }
        p
    } else {
        let r = call0(raw, "tell")?;
        ab::as_i64(&r)?
    };
    if r < 0 {
        raise!(OSError, "Raw stream returned invalid position {}", r);
    }
    Ok(r)
}

fn raw_seek(raw: &PyObject, pos: i64, whence: i32) -> PyResult<i64> {
    let r = if fileio::is_exact_fileio(raw) && fileio::fio(raw).fd >= 0 {
        let p = unsafe { libc::lseek(fileio::fio(raw).fd, pos, whence) };
        if p < 0 {
            return Err(crate::modules::posix::path::os_err());
        }
        p
    } else {
        let r = ab::call_method_str(raw, "seek", &[&int::new_int(pos), &int::new_int(whence as i64)])?;
        ab::as_i64(&r)?
    };
    if r < 0 {
        raise!(OSError, "Raw stream returned invalid position {}", r);
    }
    Ok(r)
}

/// Write out all pending data. Raises BlockingIOError if the raw stream would block.
fn flush_writes(raw: &PyObject, st: &mut State) -> PyResult<()> {
    let mut written = 0;
    while written < st.wbuf.len() {
        match raw_write(raw, &st.wbuf[written..]) {
            Ok(Some(n)) => written += n,
            Ok(None) => {
                st.wbuf.drain(..written);
                return Err(blocking_error(0));
            }
            Err(e) => {
                st.wbuf.drain(..written);
                return Err(e);
            }
        }
        crate::modules::posix::path::check_signals().map_err(|e| {
            e
        })?;
    }
    st.wbuf.clear();
    Ok(())
}

fn blocking_error(written: usize) -> PyErr {
    match crate::types::exceptions::make_exception(
        crate::types::exceptions::BlockingIOError(),
        &[
            int::new_int(libc::EAGAIN as i64),
            pystr::new_str("write could not complete without blocking"),
            int::new_int(written as i64),
        ],
    ) {
        Ok(e) => {
            set_exc(e);
            PyErr
        }
        Err(e) => e,
    }
}

/// Undo read-ahead: position raw at the logical position and drop the read buffer.
fn rewind_read(raw: &PyObject, st: &mut State) -> PyResult<()> {
    let unread = st.unread();
    if unread > 0 {
        raw_seek(raw, -(unread as i64), 1)?;
    }
    st.reset_read();
    Ok(())
}

/// Fill the (empty) read buffer with one raw read. Returns None if would block.
fn fill_buffer(raw: &PyObject, st: &mut State) -> PyResult<Option<usize>> {
    st.reset_read();
    let mut buf = std::mem::take(&mut st.rbuf);
    buf.resize(st.buffer_size, 0);
    let r = raw_readinto(raw, &mut buf);
    match r {
        Ok(Some(n)) => {
            buf.truncate(n);
            st.rbuf = buf;
            Ok(Some(n))
        }
        Ok(None) => {
            buf.clear();
            st.rbuf = buf;
            Ok(None)
        }
        Err(e) => {
            buf.clear();
            st.rbuf = buf;
            Err(e)
        }
    }
}

// ---------------------------------------------------------------- construction

unsafe extern "C" fn buffered_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe { (*tp).tp_alloc.unwrap()(tp, 0) }
}

fn kind_of(o: &PyObject) -> Kind {
    let t = o.type_ptr();
    if crate::types::typeobject::is_subtype(t, bufrandom_type()) {
        Kind::Random
    } else if crate::types::typeobject::is_subtype(t, bufwriter_type()) {
        Kind::Writer
    } else {
        Kind::Reader
    }
}

fn buffered_init_impl(o: &PyObject, raw: &PyObject, size: Option<&PyObject>) -> PyResult<()> {
    let kind = kind_of(o);
    let b = bf(o);
    // drop previous state
    if !b.st.is_null() {
        unsafe { drop(Box::from_raw(b.st)) };
        b.st = std::ptr::null_mut();
    }
    unsafe { clear_slot(&mut b.raw) };
    b.detached = false;
    if kind != Kind::Writer {
        super::iobase::check_readable(raw)?;
    }
    if kind != Kind::Reader {
        super::iobase::check_writable(raw)?;
    }
    if kind == Kind::Random {
        super::iobase::check_seekable(raw)?;
    }
    let bs = match size {
        None => DEFAULT_BUFFER_SIZE as isize,
        Some(s) => ssize_arg(s)?,
    };
    if bs <= 0 {
        raise!(ValueError, "buffer size must be strictly positive");
    }
    let b = bf(o);
    unsafe { set_slot(&mut b.raw, Some(raw)) };
    b.kind = kind as u8;
    b.st = Box::into_raw(Box::new(State {
        buffer_size: bs as usize,
        rbuf: Vec::new(),
        rpos: 0,
        wbuf: Vec::new(),
        readable: kind != Kind::Writer,
        writable: kind != Kind::Reader,
    }));
    Ok(())
}

unsafe extern "C" fn buffered_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let o = &*op;
        let r = (|| -> PyResult<()> {
            let (vals, kws) = init_args(args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            let name = crate::types::typeobject::type_name(&*(*op).type_ptr());
            let base_name = match kind_of(o) {
                Kind::Reader => "BufferedReader",
                Kind::Writer => "BufferedWriter",
                Kind::Random => "BufferedRandom",
            };
            let _ = name;
            let [raw, size] = kwparse::<2>(base_name, &["raw", "buffer_size"], 0, 1, &refs, kws.as_deref())?;
            buffered_init_impl(o, raw.unwrap(), size)
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn buffered_dealloc(op: *mut PyObject) {
    unsafe {
        if dealloc_begin(op) {
            return;
        }
        let b = &mut *(op as *mut Buffered);
        if !b.st.is_null() {
            drop(Box::from_raw(b.st));
            b.st = std::ptr::null_mut();
        }
        clear_slot(&mut b.raw);
        dealloc_end(op);
    }
}
unsafe extern "C" fn buffered_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let r = visit_header(op, visit, arg);
        if r != 0 {
            return r;
        }
        visit_ptr((*(op as *mut Buffered)).raw, visit, arg)
    }
}
unsafe extern "C" fn buffered_clear(op: *mut PyObject) -> c_int {
    unsafe {
        clear_slot(&mut hdr(&*op).dict);
        clear_slot(&mut (*(op as *mut Buffered)).raw);
        0
    }
}

// ---------------------------------------------------------------- reading

fn read_all(raw: &PyObject, st: &mut State) -> PyResult {
    let mut out: Vec<u8> = st.rbuf[st.rpos..].to_vec();
    st.reset_read();
    // prefer raw.readall()
    let readall = if fileio::is_exact_fileio(raw) { None } else { ab::getattr_opt(raw, &pystr::intern_str("readall"))? };
    if fileio::is_exact_fileio(raw) {
        let f = fileio::fio(raw);
        match fileio::readall_fd(f)? {
            Some(v) => out.extend_from_slice(&v),
            None => {
                if out.is_empty() {
                    return Ok(none::none_ref());
                }
            }
        }
        return Ok(new_bytes(&out));
    }
    if let Some(ra) = readall {
        let data = retry_interrupted(|| ab::call(&ra, &[]))?;
        if data.is(none::none()) {
            if out.is_empty() {
                return Ok(data);
            }
            return Ok(new_bytes(&out));
        }
        if !bytes::is_bytes(&data) {
            raise!(TypeError, "readall() should return bytes");
        }
        if out.is_empty() {
            return Ok(data);
        }
        out.extend_from_slice(bytes::bytes_data(&data));
        return Ok(new_bytes(&out));
    }
    loop {
        let data = retry_interrupted(|| call0(raw, "read"))?;
        if data.is(none::none()) {
            if out.is_empty() {
                return Ok(data);
            }
            break;
        }
        if !bytes::is_bytes(&data) {
            raise!(TypeError, "read() should return bytes");
        }
        let d = bytes::bytes_data(&data);
        if d.is_empty() {
            break;
        }
        out.extend_from_slice(d);
    }
    Ok(new_bytes(&out))
}

/// Read exactly up to n bytes (fewer at EOF / would-block).
fn read_n(raw: &PyObject, st: &mut State, n: usize) -> PyResult<Option<Vec<u8>>> {
    let avail = st.unread();
    if n <= avail {
        let v = st.rbuf[st.rpos..st.rpos + n].to_vec();
        st.rpos += n;
        return Ok(Some(v));
    }
    let mut out = crate::errors::try_vec_with_capacity(n)?;
    out.extend_from_slice(&st.rbuf[st.rpos..]);
    st.reset_read();
    while out.len() < n {
        let remaining = n - out.len();
        if remaining >= st.buffer_size {
            // read directly in multiples of the buffer size
            let chunk = remaining - remaining % st.buffer_size;
            let start = out.len();
            out.resize(start + chunk, 0);
            match raw_readinto(raw, &mut out[start..])? {
                Some(0) => {
                    out.truncate(start);
                    break;
                }
                Some(k) => out.truncate(start + k),
                None => {
                    out.truncate(start);
                    if out.is_empty() {
                        return Ok(None);
                    }
                    break;
                }
            }
        } else {
            match fill_buffer(raw, st)? {
                Some(0) => break,
                Some(k) => {
                    let take = k.min(remaining);
                    out.extend_from_slice(&st.rbuf[..take]);
                    st.rpos = take;
                }
                None => {
                    if out.is_empty() {
                        return Ok(None);
                    }
                    break;
                }
            }
        }
    }
    Ok(Some(out))
}

fn prepare_read(o: &PyObject) -> PyResult<(PyRef, &mut State)> {
    let (raw, st) = parts(o)?;
    if !st.readable {
        return Err(unsupported("read"));
    }
    check_closed_msg(&raw, "read of closed file")?;
    if !st.wbuf.is_empty() {
        flush_writes(&raw, st)?;
    }
    Ok((raw, st))
}

fn b_read(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("read", args, 0, 1)?;
    let n = size_arg(args.first().copied())?;
    if n < -1 {
        raise!(ValueError, "read length must be non-negative or -1");
    }
    let _g = lock(o)?;
    let (raw, st) = prepare_read(o)?;
    if n == -1 {
        return read_all(&raw, st);
    }
    match read_n(&raw, st, n as usize)? {
        Some(v) => Ok(new_bytes(&v)),
        None => Ok(none::none_ref()),
    }
}

fn b_read1(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("read1", args, 0, 1)?;
    let n = size_arg(args.first().copied())?;
    let _g = lock(o)?;
    let (raw, st) = prepare_read(o)?;
    let n = if n < 0 { st.buffer_size } else { n as usize };
    if n == 0 {
        return Ok(new_bytes(&[]));
    }
    let avail = st.unread();
    if avail > 0 {
        let k = n.min(avail);
        let v = new_bytes(&st.rbuf[st.rpos..st.rpos + k]);
        st.rpos += k;
        return Ok(v);
    }
    st.reset_read();
    if n > st.buffer_size {
        let mut buf = crate::errors::try_zeroed_vec(n)?;
        return match raw_readinto(&raw, &mut buf)? {
            Some(k) => Ok(new_bytes(&buf[..k])),
            None => Ok(none::none_ref()),
        };
    }
    match fill_buffer(&raw, st)? {
        Some(k) => {
            let take = k.min(n);
            st.rpos = take;
            Ok(new_bytes(&st.rbuf[..take]))
        }
        None => Ok(none::none_ref()),
    }
}

fn b_peek(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("peek", args, 0, 1)?;
    let _g = lock(o)?;
    let (raw, st) = prepare_read(o)?;
    if st.unread() == 0 {
        fill_buffer(&raw, st)?;
    }
    Ok(new_bytes(&st.rbuf[st.rpos..]))
}

fn readinto_impl(o: &PyObject, b: &PyObject, one: bool) -> PyResult {
    let mut view = BufView::get_msg(b, true, Some(if one { "readinto1() argument" } else { "readinto() argument" }))?;
    let _g = lock(o)?;
    let (raw, st) = prepare_read(o)?;
    let dst = view.as_mut_slice();
    let n = dst.len();
    let mut written = 0;
    let avail = st.unread();
    let take = avail.min(n);
    dst[..take].copy_from_slice(&st.rbuf[st.rpos..st.rpos + take]);
    st.rpos += take;
    written += take;
    // readinto1 adds one raw read only when the rest would not fit the buffer
    if written == n || (one && written > 0 && n - written <= st.buffer_size) {
        return Ok(int::new_int(written as i64));
    }
    st.reset_read();
    loop {
        let remaining = n - written;
        if remaining == 0 {
            break;
        }
        let r = if remaining > st.buffer_size {
            raw_readinto(&raw, &mut dst[written..])?
        } else {
            match fill_buffer(&raw, st)? {
                Some(k) => {
                    let t = k.min(remaining);
                    dst[written..written + t].copy_from_slice(&st.rbuf[..t]);
                    st.rpos = t;
                    Some(t)
                }
                None => None,
            }
        };
        match r {
            None => {
                if written == 0 {
                    return Ok(none::none_ref());
                }
                break;
            }
            Some(0) => break,
            Some(k) => {
                if remaining > st.buffer_size {
                    written += k;
                } else {
                    written += k;
                }
            }
        }
        if one {
            break;
        }
    }
    Ok(int::new_int(written as i64))
}

fn b_readinto(o: &PyObject, b: &PyObject) -> PyResult {
    readinto_impl(o, b, false)
}
fn b_readinto1(o: &PyObject, b: &PyObject) -> PyResult {
    readinto_impl(o, b, true)
}

fn readline_impl(o: &PyObject, limit: isize) -> PyResult {
    let _g = lock(o)?;
    let (raw, st) = prepare_read(o)?;
    let mut out: Vec<u8> = Vec::new();
    loop {
        let avail = &st.rbuf[st.rpos..];
        let room = if limit < 0 { usize::MAX } else { limit as usize - out.len() };
        let scan = &avail[..avail.len().min(room)];
        if let Some(i) = scan.iter().position(|&c| c == b'\n') {
            out.extend_from_slice(&scan[..=i]);
            st.rpos += i + 1;
            break;
        }
        out.extend_from_slice(scan);
        st.rpos += scan.len();
        if limit >= 0 && out.len() >= limit as usize {
            break;
        }
        match fill_buffer(&raw, st)? {
            Some(0) | None => break,
            Some(_) => {}
        }
    }
    Ok(new_bytes(&out))
}

fn b_readline(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("readline", args, 0, 1)?;
    let limit = size_arg(args.first().copied())?;
    readline_impl(o, limit)
}

unsafe extern "C" fn b_iternext(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let exact = o.is_type(bufreader_type()) || o.is_type(bufrandom_type());
        let r = if exact { readline_impl(o, -1) } else { call0(o, "readline") };
        match r {
            Ok(l) => {
                if !bytes::is_bytes(&l) {
                    let _ = err!(OSError, "readline() should have returned a bytes object, not '{}'", tname(&l));
                    return std::ptr::null_mut();
                }
                if bytes::bytes_data(&l).is_empty() {
                    std::ptr::null_mut()
                } else {
                    l.into_raw()
                }
            }
            Err(_) => std::ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------- writing

fn b_write(o: &PyObject, data: &PyObject) -> PyResult {
    let view = BufView::get(data, false)?;
    let _g = lock(o)?;
    let (raw, st) = parts(o)?;
    if !st.writable {
        return Err(unsupported("write"));
    }
    if raw_closed(&raw)? {
        raise!(ValueError, "write to closed file");
    }
    if st.unread() > 0 || !st.rbuf.is_empty() {
        if st.readable {
            rewind_read(&raw, st)?;
        }
    }
    let d = view.as_slice();
    st.wbuf.extend_from_slice(d);
    if st.wbuf.len() > st.buffer_size {
        let before = st.wbuf.len();
        match flush_writes(&raw, st) {
            Ok(()) => {}
            Err(e) => {
                if exc_matches(crate::types::exceptions::BlockingIOError()) {
                    clear_exc();
                    // keep at most buffer_size bytes pending; report what was accepted
                    if st.wbuf.len() > st.buffer_size {
                        let overflow = st.wbuf.len() - st.buffer_size;
                        st.wbuf.truncate(st.buffer_size);
                        let accepted = d.len().saturating_sub(overflow);
                        let _ = before;
                        return Err(blocking_error(accepted));
                    }
                    return Ok(int::new_int(d.len() as i64));
                }
                return Err(e);
            }
        }
    }
    Ok(int::new_int(d.len() as i64))
}

fn flush_impl(o: &PyObject) -> PyResult<()> {
    let (raw, st) = parts(o)?;
    if raw_closed(&raw)? {
        raise!(ValueError, "flush of closed file");
    }
    if st.writable && !st.wbuf.is_empty() {
        flush_writes(&raw, st)?;
    }
    if st.readable && st.writable {
        if st.unread() > 0 {
            rewind_read(&raw, st)?;
        }
        // flush() always drops the read buffer of a random-access stream
        st.reset_read();
    }
    Ok(())
}

fn b_flush(o: &PyObject) -> PyResult {
    // A reader has nothing of its own to flush: forward to the raw stream
    // without taking the buffer lock (flush() may come from inside a read).
    {
        let (raw, st) = parts(o)?;
        if !st.writable {
            return call0(&raw, "flush");
        }
    }
    let _g = lock(o)?;
    flush_impl(o)?;
    Ok(none::none_ref())
}

// ---------------------------------------------------------------- positioning

fn b_tell(o: &PyObject) -> PyResult {
    let (raw, st) = parts(o)?;
    let pos = raw_tell(&raw)?;
    let pos = pos - st.unread() as i64 + st.wbuf.len() as i64;
    Ok(int::new_int(pos.max(0)))
}

fn b_seek(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("seek", args, 1, 2)?;
    if crate::types::float::is_float(args[0]) {
        raise!(TypeError, "an integer is required");
    }
    let pos = i64_arg(args[0])?;
    let whence = if args.len() > 1 { crate::modules::posix::path::c_int(args[1])? } else { 0 };
    if !(0..=2).contains(&whence) && whence != libc::SEEK_DATA && whence != libc::SEEK_HOLE {
        raise!(ValueError, "whence value {} unsupported", whence);
    }
    let _g = lock(o)?;
    let (raw, st) = parts(o)?;
    check_closed_msg(&raw, "seek of closed file")?;
    super::iobase::check_seekable(&raw)?;
    // fast path: target inside the read buffer
    if (whence == 0 || whence == 1) && st.wbuf.is_empty() && !st.rbuf.is_empty() {
        let cur_raw = raw_tell(&raw)?;
        let buf_start = cur_raw - st.rbuf.len() as i64;
        let target = if whence == 0 { pos } else { cur_raw - st.unread() as i64 + pos };
        if target >= 0 && target >= buf_start && target <= cur_raw {
            st.rpos = (target - buf_start) as usize;
            return Ok(int::new_int(target));
        }
    }
    if !st.wbuf.is_empty() {
        flush_writes(&raw, st)?;
    }
    let mut p = pos;
    if whence == 1 {
        p -= st.unread() as i64;
    }
    st.reset_read();
    let r = raw_seek(&raw, p, whence)?;
    Ok(int::new_int(r))
}

fn b_truncate(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("truncate", args, 0, 1)?;
    let _g = lock(o)?;
    let (raw, st) = parts(o)?;
    check_closed_msg(&raw, "truncate of closed file")?;
    if !st.writable {
        return Err(unsupported("truncate"));
    }
    flush_impl(o)?;
    let (raw, st) = parts(o)?;
    let pos = match args.first() {
        Some(p) if !p.is(none::none()) => p.to_ref(),
        _ => {
            let cur = raw_tell(&raw)? - st.unread() as i64;
            int::new_int(cur)
        }
    };
    st.reset_read();
    call1(&raw, "truncate", &pos)
}

// ---------------------------------------------------------------- misc

fn b_close(o: &PyObject) -> PyResult {
    let raw = {
        let _g = lock(o)?;
        let (raw, _) = parts(o)?;
        if raw_closed(&raw)? {
            return Ok(none::none_ref());
        }
        raw
    };
    if bf(o).finalizing || hdr(o).ioflags & F_FINALIZING != 0 {
        if let Ok(Some(dw)) = ab::getattr_opt(&raw, &pystr::intern_str("_dealloc_warn")) {
            if ab::call(&dw, &[o]).is_err() {
                clear_exc();
            }
        } else {
            clear_exc();
        }
    }
    let flush_res = call0(o, "flush");
    let flush_exc = if flush_res.is_err() { take_exc() } else { None };
    // the raw stream is closed under the buffer lock: a write() from another
    // thread waits and then sees a closed file
    let guard = match lock(o) {
        Ok(g) => g,
        Err(e) => return Err(e),
    };
    {
        let b = bf(o);
        if !b.st.is_null() {
            let st = unsafe { &mut *b.st };
            st.reset_read();
        }
    }
    let close_res = call0(&raw, "close");
    drop(guard);
    match (close_res, flush_exc) {
        (Ok(_), None) => Ok(none::none_ref()),
        (Ok(_), Some(fe)) => {
            set_exc(fe);
            Err(PyErr)
        }
        (Err(e), None) => Err(e),
        (Err(e), Some(fe)) => {
            if let Some(ce) = take_exc() {
                crate::types::exceptions::set_context_and_cause(&ce, Some(&fe));
                set_exc(ce);
            }
            Err(e)
        }
    }
}

fn b_detach(o: &PyObject) -> PyResult {
    let _ = parts(o)?;
    call0(o, "flush")?;
    let b = bf(o);
    let raw = std::mem::replace(&mut b.raw, std::ptr::null_mut());
    b.detached = true;
    if !b.st.is_null() {
        unsafe { drop(Box::from_raw(b.st)) };
        b.st = std::ptr::null_mut();
    }
    Ok(unsafe { PyRef::from_raw(raw) })
}

macro_rules! forward {
    ($fname:ident, $meth:literal) => {
        fn $fname(o: &PyObject) -> PyResult {
            let raw = raw_of(o)?;
            call0(&raw, $meth)
        }
    };
}
forward!(b_seekable, "seekable");
forward!(b_fileno, "fileno");
forward!(b_isatty, "isatty");

fn b_readable(o: &PyObject) -> PyResult {
    let raw = raw_of(o)?;
    let st = parts(o)?.1;
    if !st.readable {
        return Ok(bool_::new_bool(false));
    }
    call0(&raw, "readable")
}
fn b_writable(o: &PyObject) -> PyResult {
    let raw = raw_of(o)?;
    let st = parts(o)?.1;
    if !st.writable {
        return Ok(bool_::new_bool(false));
    }
    call0(&raw, "writable")
}

fn b_dealloc_warn(o: &PyObject, source: &PyObject) -> PyResult {
    if let Ok(raw) = raw_of(o) {
        if let Some(dw) = ab::getattr_opt(&raw, &pystr::intern_str("_dealloc_warn"))? {
            ab::call(&dw, &[source])?;
        }
    } else {
        clear_exc();
    }
    Ok(none::none_ref())
}

fn get_raw(o: &PyObject) -> PyResult {
    raw_of(o)
}
fn get_closed(o: &PyObject) -> PyResult {
    let raw = raw_of(o)?;
    ab::getattr(&raw, crate::name!("closed"))
}
fn get_name(o: &PyObject) -> PyResult {
    let raw = raw_of(o)?;
    ab::getattr(&raw, crate::name!("name"))
}
fn get_mode(o: &PyObject) -> PyResult {
    let raw = raw_of(o)?;
    ab::getattr_str(&raw, "mode")
}
fn get_finalizing(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(bf(o).finalizing))
}
fn set_finalizing(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        None => raise!(TypeError, "can't delete numeric/char attribute"),
        Some(v) => {
            bf(o).finalizing = ab::is_true(v)?;
            Ok(())
        }
    }
}

fn b_sizeof(o: &PyObject) -> PyResult {
    let mut n = unsafe { (*o.type_ptr()).tp_basicsize } as usize;
    let b = bf(o);
    if !b.st.is_null() {
        // a closed stream no longer owns its buffer
        let closed = match parts(o) {
            Ok((raw, _)) => raw_closed(&raw).unwrap_or_else(|_| {
                clear_exc();
                false
            }),
            Err(_) => {
                clear_exc();
                false
            }
        };
        if !closed {
            n += unsafe { (*b.st).buffer_size };
        }
    }
    Ok(int::new_int(n as i64))
}

fn b_getstate(o: &PyObject) -> PyResult {
    raise!(TypeError, "cannot pickle '{}' instances", crate::types::typeobject::type_name(o.ty()));
}

unsafe extern "C" fn buffered_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let r = (|| -> PyResult {
            let full = crate::types::typeobject::type_fqname(o.type_ptr());
            let name = match ab::getattr_opt(o, crate::name!("name")) {
                Ok(n) => n,
                Err(e) => {
                    if exc_matches(crate::types::exceptions::ValueError()) {
                        clear_exc();
                        None
                    } else {
                        return Err(e);
                    }
                }
            };
            match name {
                None => Ok(pystr::new_str(&format!("<{}>", full))),
                Some(n) => {
                    if crate::modules::reprlib_guard::enter(o) {
                        raise!(RuntimeError, "reentrant call inside {}.__repr__", full);
                    }
                    let nr = ab::repr_string(&n);
                    crate::modules::reprlib_guard::leave(o);
                    Ok(pystr::new_str(&format!("<{} name={}>", full, nr?)))
                }
            }
        })();
        into_c(r)
    }
}

/// `closed` of an exact Buffered* over an exact FileIO, without attribute lookups.
pub fn fast_closed(o: &PyObject) -> Option<bool> {
    let t = o.type_ptr();
    if t != bufreader_type() && t != bufwriter_type() && t != bufrandom_type() {
        return None;
    }
    let b = bf(o);
    if b.raw.is_null() {
        return None;
    }
    let raw = unsafe { &*b.raw };
    if !fileio::is_exact_fileio(raw) {
        return None;
    }
    Some(fileio::fio(raw).fd < 0)
}

// ---------------------------------------------------------------- BufferedRWPair

#[repr(C)]
pub struct RWPair {
    pub hdr: IoHeader,
    pub reader: *mut PyObject,
    pub writer: *mut PyObject,
}

fn pair(o: &PyObject) -> PyResult<(PyRef, PyRef)> {
    let p = unsafe { &*(o.as_ptr() as *mut RWPair) };
    if p.reader.is_null() || p.writer.is_null() {
        raise!(ValueError, "I/O operation on uninitialized object");
    }
    Ok(unsafe { (PyRef::from_borrowed(p.reader), PyRef::from_borrowed(p.writer)) })
}

unsafe extern "C" fn rwpair_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let (vals, kws) = init_args(args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            if kws.is_some() {
                raise!(TypeError, "BufferedRWPair() takes no keyword arguments");
            }
            crate::args::check_pos("BufferedRWPair", &refs, 2, 3)?;
            super::iobase::check_readable(refs[0])?;
            super::iobase::check_writable(refs[1])?;
            let size = refs.get(2).map(|x| x.to_ref()).unwrap_or_else(|| int::new_int(DEFAULT_BUFFER_SIZE as i64));
            let reader = ab::call(&type_ref(bufreader_type()), &[refs[0], &size])?;
            let writer = ab::call(&type_ref(bufwriter_type()), &[refs[1], &size])?;
            let p = &mut *(op as *mut RWPair);
            set_slot(&mut p.reader, Some(&reader));
            set_slot(&mut p.writer, Some(&writer));
            Ok(())
        })();
        int_into_c(r)
    }
}
unsafe extern "C" fn rwpair_dealloc(op: *mut PyObject) {
    unsafe {
        if dealloc_begin(op) {
            return;
        }
        let p = &mut *(op as *mut RWPair);
        clear_slot(&mut p.reader);
        clear_slot(&mut p.writer);
        dealloc_end(op);
    }
}
unsafe extern "C" fn rwpair_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let p = &*(op as *mut RWPair);
        let r = visit_header(op, visit, arg);
        if r != 0 {
            return r;
        }
        let r = visit_ptr(p.reader, visit, arg);
        if r != 0 {
            return r;
        }
        visit_ptr(p.writer, visit, arg)
    }
}
unsafe extern "C" fn rwpair_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let p = &mut *(op as *mut RWPair);
        clear_slot(&mut hdr(&*op).dict);
        clear_slot(&mut p.reader);
        clear_slot(&mut p.writer);
        0
    }
}

macro_rules! pair_fwd {
    ($fname:ident, $which:tt, $meth:literal, FAST) => {
        fn $fname(o: &PyObject, args: &[&PyObject]) -> PyResult {
            let p = pair(o)?;
            ab::call_method_str(&p.$which, $meth, args)
        }
    };
    ($fname:ident, $which:tt, $meth:literal, O) => {
        fn $fname(o: &PyObject, a: &PyObject) -> PyResult {
            let p = pair(o)?;
            ab::call_method_str(&p.$which, $meth, &[a])
        }
    };
    ($fname:ident, $which:tt, $meth:literal, NOARGS) => {
        fn $fname(o: &PyObject) -> PyResult {
            let p = pair(o)?;
            ab::call_method_str(&p.$which, $meth, &[])
        }
    };
}
pair_fwd!(p_read, 0, "read", FAST);
pair_fwd!(p_peek, 0, "peek", FAST);
pair_fwd!(p_read1, 0, "read1", FAST);
pair_fwd!(p_readinto, 0, "readinto", O);
pair_fwd!(p_readinto1, 0, "readinto1", O);
pair_fwd!(p_readline, 0, "readline", FAST);
pair_fwd!(p_write, 1, "write", O);
pair_fwd!(p_flush, 1, "flush", NOARGS);
pair_fwd!(p_readable, 0, "readable", NOARGS);
pair_fwd!(p_writable, 1, "writable", NOARGS);
pair_fwd!(p_closed, 1, "__getattribute__", NOARGS);

fn p_get_closed(o: &PyObject) -> PyResult {
    let p = pair(o)?;
    ab::getattr(&p.1, crate::name!("closed"))
}
fn p_isatty(o: &PyObject) -> PyResult {
    let p = pair(o)?;
    let w = call0(&p.1, "isatty")?;
    if ab::is_true(&w)? {
        return Ok(w);
    }
    call0(&p.0, "isatty")
}
fn p_close(o: &PyObject) -> PyResult {
    let p = pair(o)?;
    let r1 = call0(&p.1, "close");
    let e1 = if r1.is_err() { take_exc() } else { None };
    let r2 = call0(&p.0, "close");
    match (r2, e1) {
        (Ok(_), None) => Ok(none::none_ref()),
        (Ok(_), Some(e)) => {
            set_exc(e);
            Err(PyErr)
        }
        (Err(e), None) => Err(e),
        (Err(e), Some(prev)) => {
            if let Some(ce) = take_exc() {
                crate::types::exceptions::set_context_and_cause(&ce, Some(&prev));
                set_exc(ce);
            }
            Err(e)
        }
    }
}
fn p_detach(_o: &PyObject) -> PyResult {
    Err(unsupported("detach"))
}

pub fn init_types() -> PyResult<()> {
    unsafe {
        if BUFREADER_TYPE.tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        let size = std::mem::size_of::<Buffered>();
        let base = super::iobase::bufferediobase_type();
        let common_getset = || {
            crate::getset_table![
                ("raw", get_raw),
                ("closed", get_closed),
                ("name", get_name),
                ("mode", get_mode),
                ("_finalizing", get_finalizing, set_finalizing),
            ]
        };
        for (t, name, doc, kind) in [
            (bufreader_type(), c"_io.BufferedReader", "BufferedReader(raw, buffer_size=DEFAULT_BUFFER_SIZE)\n--\n\nCreate a new buffered reader using the given readable raw IO object.", Kind::Reader),
            (bufwriter_type(), c"_io.BufferedWriter", "BufferedWriter(raw, buffer_size=DEFAULT_BUFFER_SIZE)\n--\n\nA buffer for a writeable sequential RawIO object.\n\nThe constructor creates a BufferedWriter for the given writeable raw\nstream. If the buffer_size is not given, it defaults to\nDEFAULT_BUFFER_SIZE.", Kind::Writer),
            (bufrandom_type(), c"_io.BufferedRandom", "BufferedRandom(raw, buffer_size=DEFAULT_BUFFER_SIZE)\n--\n\nA buffered interface to random access streams.\n\nThe constructor creates a reader and writer for a seekable stream,\nraw, given in the first argument. If the buffer_size is omitted it\ndefaults to DEFAULT_BUFFER_SIZE.", Kind::Random),
        ] {
            setup_type(t, name, size, base, doc);
            let tt = &mut *t;
            tt.tp_new = Some(buffered_new);
            tt.tp_init = Some(buffered_init);
            tt.tp_dealloc = Some(buffered_dealloc);
            tt.tp_traverse = Some(buffered_traverse);
            tt.tp_clear = Some(buffered_clear);
            tt.tp_repr = Some(buffered_repr);
            tt.tp_getset = common_getset();
            let mut v: Vec<PyMethodDef> = Vec::new();
            let common = crate::mdefs![
                ("detach", NOARGS, b_detach),
                ("flush", NOARGS, b_flush),
                ("close", NOARGS, b_close),
                ("seekable", NOARGS, b_seekable),
                ("readable", NOARGS, b_readable),
                ("writable", NOARGS, b_writable),
                ("fileno", NOARGS, b_fileno),
                ("isatty", NOARGS, b_isatty),
                ("_dealloc_warn", O, b_dealloc_warn),
                ("tell", NOARGS, b_tell),
                ("seek", FAST, b_seek, "seek($self, target, whence=0, /)\n--\n\nMove the stream position and return the new absolute position."),
                ("truncate", FAST, b_truncate, "truncate($self, pos=None, /)\n--\n\nFlush, then resize the raw stream to pos (default: the current position)."),
                ("__sizeof__", NOARGS, b_sizeof),
                ("__getstate__", NOARGS, b_getstate),
            ];
            let reading = crate::mdefs![
                ("read", FAST, b_read, "read($self, size=-1, /)\n--\n\nRead up to size bytes; a negative size reads until EOF."),
                ("peek", FAST, b_peek, "peek($self, size=0, /)\n--\n\nReturn buffered bytes without advancing the position."),
                ("read1", FAST, b_read1, "read1($self, size=-1, /)\n--\n\nRead up to size bytes with at most one raw read."),
                ("readinto", O, b_readinto, "readinto($self, buffer, /)\n--\n\nRead bytes into a writable buffer; return the count."),
                ("readinto1", O, b_readinto1, "readinto1($self, buffer, /)\n--\n\nLike readinto() but with at most one raw read."),
                ("readline", FAST, b_readline, "readline($self, size=-1, /)\n--\n\nRead one line, or at most size bytes of it."),
            ];
            let writing = crate::mdefs![("write", O, b_write, "write($self, buffer, /)\n--\n\nBuffer the bytes for writing and return their count.")];
            let mut push = |tbl: *mut PyMethodDef| {
                let mut m = tbl;
                while !(*m).ml_name.is_null() {
                    v.push(std::ptr::read(m));
                    m = m.add(1);
                }
            };
            push(common);
            if kind != Kind::Writer {
                push(reading);
                tt.tp_iternext = Some(b_iternext);
            }
            if kind != Kind::Reader {
                push(writing);
            }
            tt.tp_methods = crate::methods::table(v);
            if kind == Kind::Random {
                tt.tp_base = bufwriter_type();
                // BufferedRandom inherits from both in CPython; we list it on the
                // writer and add the reader methods directly.
                tt.tp_base = base;
            }
            crate::types::typeobject::type_ready(t)?;
        }

        setup_type(
            bufrwpair_type(),
            c"_io.BufferedRWPair",
            std::mem::size_of::<RWPair>(),
            base,
            "BufferedRWPair(reader, writer, buffer_size=DEFAULT_BUFFER_SIZE, /)\n--\n\nA buffered reader and writer object together.\n\nA buffered reader object and buffered writer object put together to\nform a sequential IO object that can read and write. This is typically\nused with a socket or two-way pipe.",
        );
        let t = &mut *bufrwpair_type();
        t.tp_new = Some(buffered_new);
        t.tp_init = Some(rwpair_init);
        t.tp_dealloc = Some(rwpair_dealloc);
        t.tp_traverse = Some(rwpair_traverse);
        t.tp_clear = Some(rwpair_clear);
        let _ = p_closed;
        t.tp_methods = crate::mdefs![
            ("read", FAST, p_read),
            ("peek", FAST, p_peek),
            ("read1", FAST, p_read1),
            ("readinto", O, p_readinto, "readinto(buffer)\n\nRead into a buffer from the reader side."),
            ("readinto1", O, p_readinto1, "readinto1(buffer)\n\nRead into a buffer from the reader side, with at most one raw read."),
            ("readline", FAST, p_readline, "readline($self, size=-1, /)\n--\n\nRead one line from the reader side."),
            ("write", O, p_write, "write(buffer)\n\nWrite through the writer side."),
            ("flush", NOARGS, p_flush),
            ("readable", NOARGS, p_readable),
            ("writable", NOARGS, p_writable),
            ("isatty", NOARGS, p_isatty),
            ("close", NOARGS, p_close),
            ("detach", NOARGS, p_detach),
        ];
        t.tp_getset = crate::getset_table![("closed", p_get_closed)];
        crate::types::typeobject::type_ready(bufrwpair_type())?;
    }
    Ok(())
}
