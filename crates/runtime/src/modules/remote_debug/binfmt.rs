//! (tail4) `_remote_debugging.BinaryWriter` / `BinaryReader`: the compact on-disk format
//! of the sampling profiler (`python -m profiling.sampling ... --binary`, `replay`).
//!
//! File layout (all integers in native byte order, little-endian on our targets):
//!
//! ```text
//! header (64 bytes)
//!   0  u32 magic 0x54414348          4  u32 format version (1)
//!   8  u32 Python hex version        12 u64 start timestamp (us)
//!   20 u64 sampling interval (us)    28 u64 thread-sample count
//!   36 u32 distinct (thread, interpreter) pairs
//!   40 u64 string table offset       48 u64 frame table offset
//!   56 u32 compression (0 none, 1 zstd)
//!   60 u8 profiling mode (0xff unknown)  61 u8 capture flags (bit 7: recorded)  62 u16 0
//! sample records, from offset 64 up to the string table (zstd frames when compressed)
//! string table: per string, varint byte length + UTF-8 bytes
//! frame table: per frame, varints filename/funcname string ids, line code (0: no
//!   location, else zigzag(lineno)+1), zigzag(end_lineno-lineno), zigzag(column),
//!   zigzag(end_column), then one opcode byte (0xff: none)
//! optional measured-statistics block, ending in "TACHSTAT" u32 version u32 block size
//! footer (32 bytes): u32 strings, u32 frames, u64 file size, 16 bytes "TACHYON-FOOTER\0\0"
//! ```
//!
//! A record starts with u64 thread id, u32 interpreter id, u8 kind. Kind 0 repeats the
//! thread's previous stack: varint n, then n x (varint timestamp delta, u8 status). The
//! other kinds carry one sample: varint timestamp delta, u8 status, then
//! 1 (whole stack): varint depth + frame ids, innermost first;
//! 2 (frames pushed on the previous stack): varint kept, varint pushed + frame ids;
//! 3 (frames popped, then pushed): varint popped, varint pushed + frame ids.
//! Timestamp deltas are relative to the thread's previous sample (the first one to the
//! header's start timestamp).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, dict, float, int, list, none, str as pystr};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::io::{Seek, SeekFrom, Write};

const MAGIC: u32 = 0x5441_4348;
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 64;
const FOOTER_SIZE: usize = 32;
const FOOTER_TAG: &[u8; 16] = b"TACHYON-FOOTER\0\0";
const STATS_TAG: &[u8; 8] = b"TACHSTAT";
const RECORD_HEAD: usize = 13;
const KIND_REPEAT: u8 = 0;
const KIND_FULL: u8 = 1;
const KIND_PUSH: u8 = 2;
const KIND_POP_PUSH: u8 = 3;
/// Most samples a pending repeat run (and a replayed batch) holds.
const REPEAT_MAX: usize = 8192;
/// Record bytes collected before they are written out (compressed as one zstd frame).
const CHUNK: usize = 1 << 20;
const MIN_FRAME_ENTRY: u64 = 7;
const NO_OPCODE: u8 = 0xff;
const CAPTURE_KEYS: [&str; 5] = ["all_threads", "native", "gc", "opcodes", "blocking"];

// ---------------------------------------------------------------- encoding helpers

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

struct Cursor<'a> {
    d: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> Cursor<'a> {
    fn u8(&mut self) -> Option<u8> {
        if self.pos >= self.end {
            return None;
        }
        self.pos += 1;
        Some(self.d[self.pos - 1])
    }
    fn fixed<const N: usize>(&mut self) -> Option<[u8; N]> {
        if self.end - self.pos < N {
            return None;
        }
        let mut b = [0u8; N];
        b.copy_from_slice(&self.d[self.pos..self.pos + N]);
        self.pos += N;
        Some(b)
    }
    fn varint(&mut self) -> Option<u64> {
        let mut v: u64 = 0;
        let mut shift = 0;
        loop {
            let b = self.u8()?;
            if shift >= 64 {
                return None;
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
            shift += 7;
        }
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.end - self.pos < n {
            return None;
        }
        self.pos += n;
        Some(&self.d[self.pos - n..self.pos])
    }
}

fn rd_u32(d: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(d[at..at + 4].try_into().unwrap())
}
fn rd_u64(d: &[u8], at: usize) -> u64 {
    u64::from_ne_bytes(d[at..at + 8].try_into().unwrap())
}
fn rd_f64(d: &[u8], at: usize) -> f64 {
    f64::from_ne_bytes(d[at..at + 8].try_into().unwrap())
}

/// UTF-8 bytes of a str, lone surrogates kept in their 3-byte form (so any str
/// round-trips); non-str values are stored through `str()`.
fn text_bytes(o: &PyObject) -> PyResult<Vec<u8>> {
    if o.is(none::none()) {
        return Ok(Vec::new());
    }
    let s = if pystr::is_str(o) { o.to_ref() } else { ab::str(o)? };
    if let Ok(t) = pystr::as_utf8(&s) {
        return Ok(t.as_bytes().to_vec());
    }
    crate::errors::clear_exc();
    let enc = ab::call_method_str(&s, "encode", &[&pystr::new_str("utf-8"), &pystr::new_str("surrogatepass")])?;
    Ok(crate::types::bytes::bytes_data(&enc).to_vec())
}

fn text_object(b: &[u8]) -> PyResult {
    match std::str::from_utf8(b) {
        Ok(s) => Ok(pystr::new_str(s)),
        Err(_) => crate::modules::codecs_core::decode_utf8(b, "surrogatepass"),
    }
}

fn io_error(e: &std::io::Error, path: Option<&PyObject>) -> PyErr {
    match e.raw_os_error() {
        Some(errno) => crate::types::exceptions::os_error_from_errno(errno, path, None),
        None => err!(OSError, "{}", e),
    }
}

fn path_of(o: &PyObject, func: &str) -> PyResult<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let spec = crate::modules::posix::path::PathSpec { func, arg: "filename", allow_fd: false, nullable: false };
    let p = crate::modules::posix::path::path_converter(&spec, o)?;
    Ok(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(p.c.as_bytes())))
}

/// An integer argument as i128 (through `__index__`); None when it does not fit.
fn index_i128(o: &PyObject) -> PyResult<Option<i128>> {
    let i = ab::index(o)?;
    Ok(int::as_i128(&i))
}

// ---------------------------------------------------------------- the writer

#[derive(Clone, PartialEq, Eq, Hash)]
struct FrameKey {
    filename: u32,
    funcname: u32,
    /// None: the frame has no location
    loc: Option<(i64, i64, i64, i64)>,
    opcode: u8,
}

struct ThreadRun {
    prev_ts: u64,
    stack: Vec<u32>,
    pending: Vec<(u64, u8)>,
}

#[derive(PartialEq, Clone, Copy)]
enum WState {
    Open,
    Finalized,
    Closed,
    Broken,
}

#[derive(Default)]
struct WStats {
    full: u64,
    push: u64,
    pop_push: u64,
    repeat: u64,
    repeat_samples: u64,
    frames_written: u64,
    bytes_written: u64,
}

struct WriterState {
    path: PyRef,
    file: Option<std::fs::File>,
    state: WState,
    limit_reached: bool,
    interval: u64,
    start_ts: u64,
    compression: u32,
    mode: u8,
    capture: u8,
    strings: HashMap<Vec<u8>, u32>,
    string_list: Vec<Vec<u8>>,
    frames: HashMap<FrameKey, u32>,
    frame_list: Vec<FrameKey>,
    threads: HashMap<(u64, u32), usize>,
    runs: Vec<((u64, u32), ThreadRun)>,
    buf: Vec<u8>,
    header_written: bool,
    pos: u64,
    total: u64,
    cached_total: u64,
    measured: Option<(f64, f64, Option<f64>, Option<f64>)>,
    stats: WStats,
}

/// One thread of a sample, converted to plain values before anything is committed.
struct PThread {
    tid: u64,
    status: u8,
    frames: Vec<(Vec<u8>, Vec<u8>, Option<(i64, i64, i64, i64)>, u8)>,
}

enum Fail {
    /// a value the format cannot hold (OverflowError, the writer stays usable)
    Limit(String),
    /// any other exception (the writer becomes unusable)
    Err(PyErr),
}

impl From<PyErr> for Fail {
    fn from(e: PyErr) -> Fail {
        Fail::Err(e)
    }
}

fn loc_part(o: &PyObject, i: isize, default: i64) -> PyResult<i64> {
    let v = ab::getitem(o, &int::new_int(i as i64))?;
    if v.is(none::none()) {
        return Ok(default);
    }
    Ok(index_i128(&v)?.map(|x| x.clamp(i64::MIN as i128 / 4, i64::MAX as i128 / 4) as i64).unwrap_or(-1))
}

fn parse_location(o: &PyObject) -> PyResult<Option<(i64, i64, i64, i64)>> {
    if o.is(none::none()) {
        return Ok(None);
    }
    if int::is_int(o) {
        let l = index_i128(o)?.unwrap_or(0) as i64;
        return Ok(Some((l, l, -1, -1)));
    }
    let lineno = loc_part(o, 0, 0)?;
    let end = loc_part(o, 1, lineno)?;
    let col = loc_part(o, 2, -1)?;
    let end_col = loc_part(o, 3, -1)?;
    Ok(Some((lineno, end, col, end_col)))
}

fn attr_or_item(o: &PyObject, name: &str, i: i64) -> PyResult {
    match ab::getattr_str(o, name) {
        Ok(v) => Ok(v),
        Err(e) => {
            if crate::errors::exc_matches(crate::types::exceptions::AttributeError()) {
                crate::errors::clear_exc();
                ab::getitem(o, &int::new_int(i))
            } else {
                Err(e)
            }
        }
    }
}

fn parse_interp(interp: &PyObject) -> Result<(u32, Vec<PThread>), Fail> {
    let iid_obj = attr_or_item(interp, "interpreter_id", 0)?;
    let iid = match index_i128(&iid_obj)? {
        Some(v) if (0..=u32::MAX as i128).contains(&v) => v as u32,
        _ => {
            let shown = ab::repr_string(&iid_obj).unwrap_or_default();
            return Err(Fail::Limit(format!(
                "interpreter_id {} does not fit the binary format (it must be between 0 and {})",
                shown,
                u32::MAX
            )));
        }
    };
    let mut threads = Vec::new();
    let tlist = attr_or_item(interp, "threads", 1)?;
    for t in ab::collect_iterable(&tlist)? {
        let tid_obj = attr_or_item(&t, "thread_id", 0)?;
        let tid = if tid_obj.is(none::none()) {
            0
        } else {
            match index_i128(&tid_obj)? {
                Some(v) if (0..=u64::MAX as i128).contains(&v) => v as u64,
                _ => return Err(Fail::Limit("thread_id does not fit the binary format (unsigned 64-bit)".into())),
            }
        };
        let st_obj = attr_or_item(&t, "status", 1)?;
        let status = if st_obj.is(none::none()) {
            0
        } else {
            match index_i128(&st_obj)? {
                Some(v) if (0..=255).contains(&v) => v as u8,
                _ => return Err(Fail::Limit("thread status does not fit the binary format (0..255)".into())),
            }
        };
        let finfo = attr_or_item(&t, "frame_info", 2)?;
        let mut frames = Vec::new();
        if !finfo.is(none::none()) {
            for f in ab::collect_iterable(&finfo)? {
                let v = attr_or_item(&f, "filename", 0)?;
                let filename = text_bytes(&v)?;
                let v = attr_or_item(&f, "location", 1)?;
                let loc = parse_location(&v)?;
                let v = attr_or_item(&f, "funcname", 2)?;
                let funcname = text_bytes(&v)?;
                let op_obj = match ab::getattr_str(&f, "opcode") {
                    Ok(v) => v,
                    Err(_) => {
                        crate::errors::clear_exc();
                        none::none().to_ref()
                    }
                };
                let opcode = if op_obj.is(none::none()) {
                    NO_OPCODE
                } else {
                    match index_i128(&op_obj)? {
                        Some(v) if (0..255).contains(&v) => v as u8,
                        _ => NO_OPCODE,
                    }
                };
                frames.push((filename, funcname, loc, opcode));
            }
        }
        threads.push(PThread { tid, status, frames });
    }
    Ok((iid, threads))
}

impl WriterState {
    fn intern_str(&mut self, s: Vec<u8>) -> u32 {
        if let Some(&i) = self.strings.get(&s) {
            return i;
        }
        let i = self.string_list.len() as u32;
        self.string_list.push(s.clone());
        self.strings.insert(s, i);
        i
    }

    fn intern_frame(&mut self, f: (Vec<u8>, Vec<u8>, Option<(i64, i64, i64, i64)>, u8)) -> u32 {
        let filename = self.intern_str(f.0);
        let funcname = self.intern_str(f.1);
        let key = FrameKey { filename, funcname, loc: f.2, opcode: f.3 };
        if let Some(&i) = self.frames.get(&key) {
            return i;
        }
        let i = self.frame_list.len() as u32;
        self.frame_list.push(key.clone());
        self.frames.insert(key, i);
        i
    }

    fn record_head(&mut self, key: (u64, u32), kind: u8) {
        self.buf.extend_from_slice(&key.0.to_ne_bytes());
        self.buf.extend_from_slice(&key.1.to_ne_bytes());
        self.buf.push(kind);
    }

    fn flush_pending(&mut self, idx: usize) {
        let key = self.runs[idx].0;
        let pending = std::mem::take(&mut self.runs[idx].1.pending);
        if pending.is_empty() {
            return;
        }
        self.record_head(key, KIND_REPEAT);
        put_varint(&mut self.buf, pending.len() as u64);
        for (delta, status) in &pending {
            put_varint(&mut self.buf, *delta);
            self.buf.push(*status);
        }
        self.stats.repeat += 1;
        self.stats.repeat_samples += pending.len() as u64;
    }

    fn commit_thread(&mut self, iid: u32, t: PThread, ts: u64) {
        let stack: Vec<u32> = t.frames.into_iter().map(|f| self.intern_frame(f)).collect();
        let key = (t.tid, iid);
        let idx = match self.threads.get(&key) {
            Some(&i) => i,
            None => {
                self.runs.push((key, ThreadRun { prev_ts: self.start_ts, stack: Vec::new(), pending: Vec::new() }));
                self.threads.insert(key, self.runs.len() - 1);
                self.runs.len() - 1
            }
        };
        let run = &mut self.runs[idx].1;
        let delta = ts.saturating_sub(run.prev_ts);
        run.prev_ts = run.prev_ts.max(ts);
        self.total += 1;
        if run.stack == stack {
            run.pending.push((delta, t.status));
            if run.pending.len() >= REPEAT_MAX {
                self.flush_pending(idx);
            }
            return;
        }
        self.flush_pending(idx);
        let prev = std::mem::take(&mut self.runs[idx].1.stack);
        let kept = prev.iter().rev().zip(stack.iter().rev()).take_while(|(a, b)| a == b).count();
        let pushed = &stack[..stack.len() - kept];
        if kept == 0 {
            self.record_head(key, KIND_FULL);
            put_varint(&mut self.buf, delta);
            self.buf.push(t.status);
            put_varint(&mut self.buf, stack.len() as u64);
            self.stats.full += 1;
        } else if kept == prev.len() {
            self.record_head(key, KIND_PUSH);
            put_varint(&mut self.buf, delta);
            self.buf.push(t.status);
            put_varint(&mut self.buf, kept as u64);
            put_varint(&mut self.buf, pushed.len() as u64);
            self.stats.push += 1;
        } else {
            self.record_head(key, KIND_POP_PUSH);
            put_varint(&mut self.buf, delta);
            self.buf.push(t.status);
            put_varint(&mut self.buf, (prev.len() - kept) as u64);
            put_varint(&mut self.buf, pushed.len() as u64);
            self.stats.pop_push += 1;
        }
        let ids: Vec<u32> = if kept == 0 { stack.clone() } else { pushed.to_vec() };
        for id in &ids {
            put_varint(&mut self.buf, *id as u64);
        }
        self.stats.frames_written += ids.len() as u64;
        self.runs[idx].1.stack = stack;
    }

    fn write_raw(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let f = self.file.as_mut().ok_or_else(|| std::io::Error::other("file is closed"))?;
        f.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        self.stats.bytes_written += bytes.len() as u64;
        Ok(())
    }

    /// Write the collected record bytes (compressed when asked).
    fn flush_chunk(&mut self) -> std::io::Result<()> {
        if !self.header_written {
            self.write_raw(&[0u8; HEADER_SIZE])?;
            self.header_written = true;
        }
        if self.buf.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.buf);
        if self.compression == 1 {
            let z = rpy_compress::zstd::compress(&data, 3);
            self.write_raw(&z)
        } else {
            self.write_raw(&data)
        }
    }

    fn header(&self, str_off: u64, frame_off: u64) -> [u8; HEADER_SIZE] {
        let mut h = [0u8; HEADER_SIZE];
        h[0..4].copy_from_slice(&MAGIC.to_ne_bytes());
        h[4..8].copy_from_slice(&VERSION.to_ne_bytes());
        h[8..12].copy_from_slice(&0x030f_02f0u32.to_ne_bytes());
        h[12..20].copy_from_slice(&self.start_ts.to_ne_bytes());
        h[20..28].copy_from_slice(&self.interval.to_ne_bytes());
        h[28..36].copy_from_slice(&self.total.to_ne_bytes());
        h[36..40].copy_from_slice(&(self.runs.len() as u32).to_ne_bytes());
        h[40..48].copy_from_slice(&str_off.to_ne_bytes());
        h[48..56].copy_from_slice(&frame_off.to_ne_bytes());
        h[56..60].copy_from_slice(&self.compression.to_ne_bytes());
        h[60] = self.mode;
        h[61] = self.capture;
        h
    }

    fn finalize(&mut self) -> std::io::Result<()> {
        for i in 0..self.runs.len() {
            self.flush_pending(i);
        }
        self.flush_chunk()?;
        let str_off = self.pos;
        let mut out = Vec::new();
        for s in &self.string_list {
            put_varint(&mut out, s.len() as u64);
            out.extend_from_slice(s);
        }
        let frame_off = str_off + out.len() as u64;
        for f in &self.frame_list {
            put_varint(&mut out, f.filename as u64);
            put_varint(&mut out, f.funcname as u64);
            match f.loc {
                None => out.extend_from_slice(&[0, 0, 0, 0]),
                Some((l, e, c, ec)) => {
                    put_varint(&mut out, zigzag(l) + 1);
                    put_varint(&mut out, zigzag(e - l));
                    put_varint(&mut out, zigzag(c));
                    put_varint(&mut out, zigzag(ec));
                }
            }
            out.push(f.opcode);
        }
        if let Some((dur, rate, err_rate, missed)) = self.measured {
            let mut flags = 0u64;
            out.extend_from_slice(&dur.to_ne_bytes());
            out.extend_from_slice(&rate.to_ne_bytes());
            out.extend_from_slice(&err_rate.unwrap_or(0.0).to_ne_bytes());
            out.extend_from_slice(&missed.unwrap_or(0.0).to_ne_bytes());
            if err_rate.is_some() {
                flags |= 1;
            }
            if missed.is_some() {
                flags |= 2;
            }
            out.extend_from_slice(&flags.to_ne_bytes());
            out.extend_from_slice(STATS_TAG);
            out.extend_from_slice(&2u32.to_ne_bytes());
            out.extend_from_slice(&56u32.to_ne_bytes());
        }
        let file_size = str_off + out.len() as u64 + FOOTER_SIZE as u64;
        out.extend_from_slice(&(self.string_list.len() as u32).to_ne_bytes());
        out.extend_from_slice(&(self.frame_list.len() as u32).to_ne_bytes());
        out.extend_from_slice(&file_size.to_ne_bytes());
        out.extend_from_slice(FOOTER_TAG);
        self.write_raw(&out)?;
        let h = self.header(str_off, frame_off);
        let f = self.file.as_mut().unwrap();
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&h)?;
        f.flush()?;
        f.sync_data().or_else(|e| if e.raw_os_error() == Some(libc::EINVAL) { Ok(()) } else { Err(e) })?;
        Ok(())
    }
}

#[repr(C)]
struct Writer {
    ob_base: PyObject,
    inner: *mut RefCell<WriterState>,
}

static mut WRITER_TYPE: PyTypeObject = PyTypeObject::ZERO;
fn writer_type() -> *mut PyTypeObject {
    &raw mut WRITER_TYPE
}

fn ws(o: &PyObject) -> &RefCell<WriterState> {
    unsafe { &*(*(o.as_ptr() as *const Writer)).inner }
}

fn check_writable(st: &WriterState) -> PyResult<()> {
    match st.state {
        WState::Open => Ok(()),
        WState::Broken => raise!(ValueError, "BinaryWriter is broken: an earlier write or finalize failed"),
        WState::Finalized => raise!(ValueError, "BinaryWriter has already been finalized"),
        WState::Closed => raise!(ValueError, "BinaryWriter is closed"),
    }
}

unsafe extern "C" fn writer_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let w = op as *mut Writer;
        if !(*w).inner.is_null() {
            drop(Box::from_raw((*w).inner));
        }
        crate::types::typeobject::generic_free_and_type_decref(op, (*op).type_ptr());
    }
}

fn opt_u8_mode(o: Option<&PyObject>) -> PyResult<u8> {
    match o {
        None => Ok(0xff),
        Some(v) if v.is(none::none()) => Ok(0xff),
        Some(v) => match index_i128(v)? {
            Some(m) if (0..255).contains(&m) => Ok(m as u8),
            _ => raise!(ValueError, "mode must be between 0 and 254"),
        },
    }
}

fn capture_flags(o: Option<&PyObject>) -> PyResult<u8> {
    let Some(v) = o else { return Ok(0) };
    if v.is(none::none()) {
        return Ok(0);
    }
    let mut flags = 0x80u8;
    for (i, k) in CAPTURE_KEYS.iter().enumerate() {
        let got = match ab::call_method_str(v, "get", &[&pystr::new_str(k), &bool_::new_bool(false)]) {
            Ok(x) => x,
            Err(e) => return Err(e),
        };
        if ab::is_true(&got)? {
            flags |= 1 << i;
        }
    }
    Ok(flags)
}

unsafe extern "C" fn writer_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (vals, kws) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            static P: crate::args::Params = crate::args::Params::new(
                "BinaryWriter",
                &["filename", "sample_interval_us", "start_time_us", "compression", "mode", "capture_config"],
                0,
                3,
            )
            .kwonly(3);
            let parsed = crate::args::parse::<6>(&P, &refs, kws.as_deref())?;
            let path_obj = parsed[0].unwrap();
            let path = path_of(path_obj, "BinaryWriter")?;
            let interval = index_i128(parsed[1].unwrap())?.unwrap_or(0).max(0) as u64;
            let start = index_i128(parsed[2].unwrap())?.unwrap_or(0).max(0) as u64;
            let compression = match parsed[3] {
                None => 0,
                Some(c) => match index_i128(c)? {
                    Some(0) => 0,
                    Some(1) => 1,
                    _ => raise!(ValueError, "compression must be 0 (none) or 1 (zstd)"),
                },
            };
            let mode = opt_u8_mode(parsed[4])?;
            let capture = capture_flags(parsed[5])?;
            // audit event as for open(), before the file is created
            crate::modules::sys_full::audit("open", &[path_obj, &crate::types::str::new_str("wb"), &crate::types::int::new_int(0)])?;
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)
                .map_err(|e| io_error(&e, Some(path_obj)))?;
            let state = WriterState {
                path: path_obj.to_ref(),
                file: Some(file),
                state: WState::Open,
                limit_reached: false,
                interval,
                start_ts: start,
                compression,
                mode,
                capture,
                strings: HashMap::new(),
                string_list: Vec::new(),
                frames: HashMap::new(),
                frame_list: Vec::new(),
                threads: HashMap::new(),
                runs: Vec::new(),
                buf: Vec::new(),
                header_written: false,
                pos: 0,
                total: 0,
                cached_total: 0,
                measured: None,
                stats: WStats::default(),
            };
            let op = ((*tp).tp_alloc.unwrap())(tp, 0);
            if op.is_null() {
                return Err(PyErr);
            }
            (*(op as *mut Writer)).inner = Box::into_raw(Box::new(RefCell::new(state)));
            Ok(PyRef::from_raw(op))
        })();
        into_c(r)
    }
}

fn writer_write_sample(o: &PyObject, args: &[&PyObject], kwnames: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("write_sample", &["stack_frames", "timestamp_us"], 0, 2);
    let parsed = crate::args::parse::<2>(&P, args, kwnames)?;
    check_writable(&ws(o).borrow())?;
    let ts = match index_i128(parsed[1].unwrap()) {
        Ok(v) => v.unwrap_or(0).clamp(0, u64::MAX as i128) as u64,
        Err(e) => {
            ws(o).borrow_mut().state = WState::Broken;
            return Err(e);
        }
    };
    let interps = match ab::collect_iterable(parsed[0].unwrap()) {
        Ok(v) => v,
        Err(e) => {
            ws(o).borrow_mut().state = WState::Broken;
            return Err(e);
        }
    };
    for interp in interps {
        match parse_interp(&interp) {
            Ok((iid, threads)) => {
                let mut st = ws(o).borrow_mut();
                for t in threads {
                    st.commit_thread(iid, t, ts);
                }
                if st.buf.len() >= CHUNK {
                    if let Err(e) = st.flush_chunk() {
                        st.state = WState::Broken;
                        let p = st.path.clone();
                        drop(st);
                        return Err(io_error(&e, Some(&p)));
                    }
                }
            }
            Err(Fail::Limit(msg)) => {
                ws(o).borrow_mut().limit_reached = true;
                raise!(OverflowError, "{}", msg);
            }
            Err(Fail::Err(e)) => {
                ws(o).borrow_mut().state = WState::Broken;
                return Err(e);
            }
        }
    }
    Ok(none::none().to_ref())
}

fn do_finalize(o: &PyObject) -> PyResult<()> {
    let mut st = ws(o).borrow_mut();
    check_writable(&st)?;
    match st.finalize() {
        Ok(()) => {
            st.cached_total = st.total;
            st.state = WState::Finalized;
            st.file = None;
            Ok(())
        }
        Err(e) => {
            st.state = WState::Broken;
            st.file = None;
            let p = st.path.clone();
            drop(st);
            Err(io_error(&e, Some(&p)))
        }
    }
}

fn writer_finalize(o: &PyObject) -> PyResult {
    do_finalize(o)?;
    Ok(none::none().to_ref())
}

fn writer_close(o: &PyObject) -> PyResult {
    let mut st = ws(o).borrow_mut();
    st.file = None;
    if st.state == WState::Open {
        st.state = WState::Closed;
    } else if st.state == WState::Broken {
        st.state = WState::Closed;
    }
    Ok(none::none().to_ref())
}

fn writer_enter(o: &PyObject) -> PyResult {
    Ok(o.to_ref())
}

fn writer_exit(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    let state = ws(o).borrow().state;
    match state {
        WState::Open => do_finalize(o)?,
        WState::Broken => {
            let mut st = ws(o).borrow_mut();
            st.file = None;
        }
        _ => {}
    }
    Ok(bool_::new_bool(false))
}

fn opt_float(o: Option<&PyObject>) -> PyResult<Option<f64>> {
    match o {
        None => Ok(None),
        Some(v) if v.is(none::none()) => Ok(None),
        Some(v) => Ok(Some(ab::float_value(v)?)),
    }
}

fn writer_set_stats(o: &PyObject, args: &[&PyObject], kwnames: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params =
        crate::args::Params::new("set_stats", &["duration_sec", "sample_rate", "error_rate", "missed_samples"], 0, 2);
    let parsed = crate::args::parse::<4>(&P, args, kwnames)?;
    let dur = ab::float_value(parsed[0].unwrap())?;
    let rate = ab::float_value(parsed[1].unwrap())?;
    let err_rate = opt_float(parsed[2])?;
    let missed = opt_float(parsed[3])?;
    ws(o).borrow_mut().measured = Some((dur, rate, err_rate, missed));
    Ok(none::none().to_ref())
}

fn writer_get_stats(o: &PyObject) -> PyResult {
    let st = ws(o).borrow();
    let d = dict::new_dict();
    let set = |k: &str, v: u64| dict::set_item_str(&d, k, &int::new_int(v as i64));
    let total = if st.state == WState::Open { st.total } else { st.cached_total };
    set("total_samples", total)?;
    set("full_records", st.stats.full)?;
    set("suffix_records", st.stats.push)?;
    set("pop_push_records", st.stats.pop_push)?;
    set("repeat_records", st.stats.repeat)?;
    set("repeat_samples", st.stats.repeat_samples)?;
    set("frames_written", st.stats.frames_written)?;
    set("unique_strings", st.string_list.len() as u64)?;
    set("unique_frames", st.frame_list.len() as u64)?;
    set("unique_threads", st.runs.len() as u64)?;
    set("bytes_written", st.stats.bytes_written)?;
    Ok(d)
}

fn writer_total_samples(o: &PyObject) -> PyResult {
    let st = ws(o).borrow();
    let v = if st.state == WState::Open { st.total } else { st.cached_total };
    Ok(int::new_int(v as i64))
}

fn writer_limit_reached(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(ws(o).borrow().limit_reached))
}

// ---------------------------------------------------------------- the reader

struct ReaderState {
    path: PyRef,
    data: Option<Vec<u8>>,
    start_ts: u64,
    interval: u64,
    sample_count: u64,
    thread_count: u32,
    str_off: u64,
    frame_off: u64,
    compression: u32,
    mode: u8,
    capture: u8,
    py_version: u32,
    string_count: u32,
    frame_count: u32,
    measured: Option<(f64, f64, Option<f64>, Option<f64>)>,
}

#[repr(C)]
struct Reader {
    ob_base: PyObject,
    inner: *mut RefCell<ReaderState>,
}

static mut READER_TYPE: PyTypeObject = PyTypeObject::ZERO;
fn reader_type() -> *mut PyTypeObject {
    &raw mut READER_TYPE
}

fn rs(o: &PyObject) -> &RefCell<ReaderState> {
    unsafe { &*(*(o.as_ptr() as *const Reader)).inner }
}

unsafe extern "C" fn reader_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let r = op as *mut Reader;
        if !(*r).inner.is_null() {
            drop(Box::from_raw((*r).inner));
        }
        crate::types::typeobject::generic_free_and_type_decref(op, (*op).type_ptr());
    }
}

fn open_reader(path_obj: &PyObject) -> PyResult<ReaderState> {
    let path = path_of(path_obj, "BinaryReader")?;
    crate::modules::sys_full::audit("open", &[path_obj, &crate::types::str::new_str("rb"), &crate::types::int::new_int(0)])?;
    let data = std::fs::read(&path).map_err(|e| io_error(&e, Some(path_obj)))?;
    let size = data.len();
    if size < HEADER_SIZE + FOOTER_SIZE {
        if size >= 4 && rd_u32(&data, 0) != MAGIC {
            raise!(ValueError, "Invalid magic number: not a binary sampling profile");
        }
        raise!(ValueError, "File too small to be a binary sampling profile ({} bytes)", size);
    }
    if rd_u32(&data, 0) != MAGIC {
        raise!(ValueError, "Invalid magic number: not a binary sampling profile");
    }
    let version = rd_u32(&data, 4);
    if version != VERSION {
        raise!(ValueError, "Unsupported format version {}", version);
    }
    let foot = size - FOOTER_SIZE;
    let string_count = rd_u32(&data, foot);
    let frame_count = rd_u32(&data, foot + 4);
    let recorded_size = rd_u64(&data, foot + 8);
    if recorded_size != size as u64 {
        raise!(ValueError, "File size mismatch: the footer records {} bytes but the file has {}", recorded_size, size);
    }
    let str_off = rd_u64(&data, 40);
    let frame_off = rd_u64(&data, 48);
    if str_off < HEADER_SIZE as u64 || frame_off < str_off || frame_off > size as u64 {
        raise!(ValueError, "Invalid table offsets in header (strings at {}, frames at {})", str_off, frame_off);
    }
    let max_strings = size as u64 - str_off;
    if string_count as u64 > max_strings {
        raise!(ValueError, "Invalid string count {} exceeds maximum possible {}", string_count, max_strings);
    }
    let max_frames = (size as u64 - frame_off) / MIN_FRAME_ENTRY;
    if frame_count as u64 > max_frames {
        raise!(ValueError, "Invalid frame count {} exceeds maximum possible {}", frame_count, max_frames);
    }
    let compression = rd_u32(&data, 56);
    if compression > 1 {
        raise!(ValueError, "Unsupported compression type {}", compression);
    }
    // measured statistics, just before the footer
    let mut measured = None;
    if foot >= 16 && &data[foot - 16..foot - 8] == STATS_TAG {
        let sver = rd_u32(&data, foot - 8);
        let ssize = rd_u32(&data, foot - 4) as usize;
        if ssize >= 32 && ssize <= foot && foot - ssize >= frame_off as usize {
            let b = foot - ssize;
            let dur = rd_f64(&data, b);
            let rate = rd_f64(&data, b + 8);
            if sver >= 2 && ssize >= 56 {
                let flags = rd_u64(&data, b + 32);
                let er = rd_f64(&data, b + 16);
                let ms = rd_f64(&data, b + 24);
                measured = Some((dur, rate, (flags & 1 != 0).then_some(er), (flags & 2 != 0).then_some(ms)));
            } else {
                measured = Some((dur, rate, None, None));
            }
        }
    }
    Ok(ReaderState {
        path: path_obj.to_ref(),
        start_ts: rd_u64(&data, 12),
        interval: rd_u64(&data, 20),
        sample_count: rd_u64(&data, 28),
        thread_count: rd_u32(&data, 36),
        str_off,
        frame_off,
        compression,
        mode: data[60],
        capture: data[61],
        py_version: rd_u32(&data, 8),
        string_count,
        frame_count,
        measured,
        data: Some(data),
    })
}

unsafe extern "C" fn reader_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (vals, kws) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            static P: crate::args::Params = crate::args::Params::new("BinaryReader", &["filename"], 0, 1);
            let parsed = crate::args::parse::<1>(&P, &refs, kws.as_deref())?;
            let state = open_reader(parsed[0].unwrap())?;
            let op = ((*tp).tp_alloc.unwrap())(tp, 0);
            if op.is_null() {
                return Err(PyErr);
            }
            (*(op as *mut Reader)).inner = Box::into_raw(Box::new(RefCell::new(state)));
            Ok(PyRef::from_raw(op))
        })();
        into_c(r)
    }
}

fn opt_float_obj(v: Option<f64>) -> PyRef {
    match v {
        Some(x) => float::new_float(x),
        None => none::none().to_ref(),
    }
}

fn reader_get_info(o: &PyObject) -> PyResult {
    let st = rs(o).borrow();
    if st.data.is_none() {
        raise!(RuntimeError, "BinaryReader is closed");
    }
    let d = dict::new_dict();
    let seti = |k: &str, v: u64| dict::set_item_str(&d, k, &int::new_int(v as i64));
    seti("version", VERSION as u64)?;
    let pv = st.py_version;
    dict::set_item_str(
        &d,
        "python_version",
        &pystr::new_str(&format!("{}.{}.{}", pv >> 24, (pv >> 16) & 0xff, (pv >> 8) & 0xff)),
    )?;
    seti("start_time_us", st.start_ts)?;
    seti("sample_interval_us", st.interval)?;
    dict::set_item_str(&d, "sample_count", &int::new_int_u128(st.sample_count as u128))?;
    seti("thread_count", st.thread_count as u64)?;
    seti("string_count", st.string_count as u64)?;
    seti("frame_count", st.frame_count as u64)?;
    seti("compression_type", st.compression as u64)?;
    dict::set_item_str(&d, "compression", &pystr::new_str(if st.compression == 1 { "zstd" } else { "none" }))?;
    let mode = if st.mode == 0xff { none::none().to_ref() } else { int::new_int(st.mode as i64) };
    dict::set_item_str(&d, "mode", &mode)?;
    if st.capture & 0x80 != 0 {
        let c = dict::new_dict();
        for (i, k) in CAPTURE_KEYS.iter().enumerate() {
            dict::set_item_str(&c, k, &bool_::new_bool(st.capture & (1 << i) != 0))?;
        }
        dict::set_item_str(&d, "capture_config", &c)?;
    } else {
        dict::set_item_str(&d, "capture_config", none::none())?;
    }
    let (dur, rate, er, ms) = match st.measured {
        Some((a, b, c, e)) => (Some(a), Some(b), c, e),
        None => (None, None, None, None),
    };
    dict::set_item_str(&d, "duration_sec", &opt_float_obj(dur))?;
    dict::set_item_str(&d, "sample_rate", &opt_float_obj(rate))?;
    dict::set_item_str(&d, "error_rate", &opt_float_obj(er))?;
    dict::set_item_str(&d, "missed_samples", &opt_float_obj(ms))?;
    Ok(d)
}

struct Tables {
    frames: Vec<PyRef>,
}

fn load_tables(st: &ReaderState, data: &[u8]) -> PyResult<Tables> {
    let mut c = Cursor { d: data, pos: st.str_off as usize, end: st.frame_off as usize };
    let mut strings = Vec::with_capacity(st.string_count as usize);
    for _ in 0..st.string_count {
        let Some(n) = c.varint() else { raise!(ValueError, "Truncated string table") };
        let Some(b) = c.bytes(n as usize) else { raise!(ValueError, "Truncated string table") };
        strings.push(text_object(b)?);
    }
    let footer = data.len() - FOOTER_SIZE;
    let mut c = Cursor { d: data, pos: st.frame_off as usize, end: footer.max(st.frame_off as usize) };
    let mut frames = Vec::with_capacity(st.frame_count as usize);
    let fi = super::reader::frame_info_type();
    let li = super::reader::location_info_type();
    for _ in 0..st.frame_count {
        let vals = (|| {
            Some((c.varint()?, c.varint()?, c.varint()?, c.varint()?, c.varint()?, c.varint()?, c.u8()?))
        })();
        let Some((fname, func, lcode, dend, col, ecol, op)) = vals else {
            raise!(ValueError, "Truncated frame table")
        };
        if fname as usize >= strings.len() || func as usize >= strings.len() {
            raise!(ValueError, "Invalid string index in frame table");
        }
        let loc = if lcode == 0 {
            none::none().to_ref()
        } else {
            let l = unzigzag(lcode - 1);
            let e = l + unzigzag(dend);
            crate::types::structseq::new_instance(
                li,
                vec![int::new_int(l), int::new_int(e), int::new_int(unzigzag(col)), int::new_int(unzigzag(ecol))],
            )
        };
        let opcode = if op == NO_OPCODE { none::none().to_ref() } else { int::new_int(op as i64) };
        frames.push(crate::types::structseq::new_instance(
            fi,
            vec![strings[fname as usize].clone(), loc, strings[func as usize].clone(), opcode],
        ));
    }
    Ok(Tables { frames })
}

struct RThread {
    prev_ts: u64,
    stack: Vec<u32>,
}

fn build_sample(t: &Tables, tid: u64, iid: u32, status: u8, stack: &[u32]) -> PyRef {
    let frames: Vec<PyRef> = stack.iter().map(|&i| t.frames[i as usize].clone()).collect();
    let thread = crate::types::structseq::new_instance(
        super::reader::thread_info_type(),
        vec![int::new_int_u128(tid as u128), int::new_int(status as i64), list::new_list(frames)],
    );
    let interp = crate::types::structseq::new_instance(
        super::reader::interp_info_type(),
        vec![int::new_int(iid as i64), list::new_list(vec![thread])],
    );
    list::new_list(vec![interp])
}

fn reader_replay(o: &PyObject, args: &[&PyObject], kwnames: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("replay", &["collector", "progress_callback"], 0, 1);
    let parsed = crate::args::parse::<2>(&P, args, kwnames)?;
    let collector = parsed[0].unwrap();
    let progress = parsed[1].filter(|p| !p.is(none::none()));
    // copy what is needed so no borrow is held while Python code runs
    let (data, sample_count, thread_count, start_ts, tables, records) = {
        let st = rs(o).borrow();
        let Some(data) = st.data.as_ref() else { raise!(RuntimeError, "BinaryReader is closed") };
        let tables = load_tables(&st, data)?;
        let region = &data[HEADER_SIZE.min(st.str_off as usize)..st.str_off as usize];
        let records = if st.compression == 1 && !region.is_empty() {
            match rpy_compress::zstd::decompress(region) {
                Ok(v) => v,
                Err(_) => raise!(ValueError, "Failed to decompress the sample data"),
            }
        } else {
            region.to_vec()
        };
        (data.len(), st.sample_count, st.thread_count, st.start_ts, tables, records)
    };
    let _ = data;
    let collect_name = pystr::new_str("collect");
    let mut threads: HashMap<(u64, u32), RThread> = HashMap::new();
    let mut c = Cursor { d: &records, pos: 0, end: records.len() };
    let mut count: u64 = 0;
    let nframes = tables.frames.len() as u64;
    let mut emit = |tid: u64, iid: u32, status: u8, stack: &[u32], stamps: &[u64]| -> PyResult<()> {
        let sample = build_sample(&tables, tid, iid, status, stack);
        let ts: Vec<PyRef> = stamps.iter().map(|&t| int::new_int_u128(t as u128)).collect();
        ab::call_method(collector, &collect_name, &[&sample, &list::new_list(ts)])?;
        Ok(())
    };
    while c.pos < c.end {
        let left = c.end - c.pos;
        if left < RECORD_HEAD {
            raise!(ValueError, "Truncated sample data: {} trailing bytes", left);
        }
        let tid = u64::from_ne_bytes(c.fixed::<8>().unwrap());
        let iid = u32::from_ne_bytes(c.fixed::<4>().unwrap());
        let kind = c.u8().unwrap();
        if !threads.contains_key(&(tid, iid)) {
            if threads.len() as u64 >= thread_count as u64 {
                raise!(
                    ValueError,
                    "Invalid thread count: sample data contains more unique threads than declared in header (declared {}, found at least {})",
                    thread_count,
                    threads.len() + 1
                );
            }
            threads.insert((tid, iid), RThread { prev_ts: start_ts, stack: Vec::new() });
        }
        let th = threads.get_mut(&(tid, iid)).unwrap();
        macro_rules! need {
            ($e:expr) => {
                match $e {
                    Some(v) => v,
                    None => raise!(ValueError, "Truncated sample record"),
                }
            };
        }
        if kind == KIND_REPEAT {
            let n = need!(c.varint());
            let stack = th.stack.clone();
            let mut batch: Vec<u64> = Vec::new();
            let mut batch_status = 0u8;
            for _ in 0..n {
                let delta = need!(c.varint());
                let status = need!(c.u8());
                let th = threads.get_mut(&(tid, iid)).unwrap();
                th.prev_ts = th.prev_ts.wrapping_add(delta);
                if !batch.is_empty() && (status != batch_status || batch.len() >= REPEAT_MAX) {
                    emit(tid, iid, batch_status, &stack, &batch)?;
                    count += batch.len() as u64;
                    batch.clear();
                }
                batch_status = status;
                batch.push(th.prev_ts);
            }
            if !batch.is_empty() {
                emit(tid, iid, batch_status, &stack, &batch)?;
                count += batch.len() as u64;
            }
            continue;
        }
        let delta = need!(c.varint());
        let status = need!(c.u8());
        let new_stack: Vec<u32> = match kind {
            KIND_FULL => {
                let depth = need!(c.varint());
                let mut s = Vec::new();
                for _ in 0..depth {
                    let id = need!(c.varint());
                    if id >= nframes {
                        raise!(ValueError, "Invalid frame index {} (the frame table has {} entries)", id, nframes);
                    }
                    s.push(id as u32);
                }
                s
            }
            KIND_PUSH | KIND_POP_PUSH => {
                let a = need!(c.varint()) as usize;
                let pushed = need!(c.varint());
                let keep_from = if kind == KIND_PUSH {
                    if a > th.stack.len() {
                        raise!(ValueError, "Invalid stack encoding: keeps {} of {} frames", a, th.stack.len());
                    }
                    th.stack.len() - a
                } else {
                    if a > th.stack.len() {
                        raise!(ValueError, "Invalid stack encoding: pops {} of {} frames", a, th.stack.len());
                    }
                    a
                };
                let mut s = Vec::new();
                for _ in 0..pushed {
                    let id = need!(c.varint());
                    if id >= nframes {
                        raise!(ValueError, "Invalid frame index {} (the frame table has {} entries)", id, nframes);
                    }
                    s.push(id as u32);
                }
                s.extend_from_slice(&th.stack[keep_from..]);
                s
            }
            other => raise!(ValueError, "Invalid stack encoding type {}", other),
        };
        th.prev_ts = th.prev_ts.wrapping_add(delta);
        let ts = th.prev_ts;
        th.stack = new_stack.clone();
        emit(tid, iid, status, &new_stack, &[ts])?;
        count += 1;
        if let Some(p) = progress {
            if count % 1000 == 0 {
                ab::call(p, &[&int::new_int(count as i64), &int::new_int_u128(sample_count as u128)])?;
            }
        }
    }
    if count != sample_count {
        raise!(ValueError, "Sample count mismatch: header declares {} samples but replay decoded {}", sample_count, count);
    }
    if let Some(p) = progress {
        ab::call(p, &[&int::new_int(count as i64), &int::new_int_u128(sample_count as u128)])?;
    }
    Ok(int::new_int_u128(count as u128))
}

fn reader_close(o: &PyObject) -> PyResult {
    rs(o).borrow_mut().data = None;
    Ok(none::none().to_ref())
}

fn reader_enter(o: &PyObject) -> PyResult {
    Ok(o.to_ref())
}

fn reader_exit(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    reader_close(o)?;
    Ok(bool_::new_bool(false))
}

unsafe extern "C" fn empty_traverse(_op: *mut PyObject, _visit: visitproc, _arg: *mut c_void) -> std::ffi::c_int {
    0
}

/// Ready both types and add them to the module dict.
pub fn init(d: &PyObject) -> PyResult<()> {
    unsafe {
        let t = &mut *writer_type();
        t.tp_name = crate::methods::cstr_static("_remote_debugging.BinaryWriter");
        t.tp_basicsize = std::mem::size_of::<Writer>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_new = Some(writer_new);
        t.tp_dealloc = Some(writer_dealloc);
        t.tp_traverse = Some(empty_traverse);
        t.tp_doc = crate::methods::cstr_static(
            "BinaryWriter(filename, sample_interval_us, start_time_us, *, compression=0, mode=None, capture_config=None)\n--\n\nWrite profiler samples in the compact binary format.",
        );
        t.tp_methods = crate::mdefs![
            ("write_sample", FASTKW, writer_write_sample, "write_sample($self, /, stack_frames, timestamp_us)\n--\n\nAppend one sample (a list of InterpreterInfo)."),
            ("finalize", NOARGS, writer_finalize, "finalize($self, /)\n--\n\nWrite the tables, footer and header and close the file."),
            ("close", NOARGS, writer_close, "close($self, /)\n--\n\nClose the file without finalizing it."),
            ("set_stats", FASTKW, writer_set_stats, "set_stats($self, /, duration_sec, sample_rate, error_rate=None, missed_samples=None)\n--\n\nRecord the measured run statistics."),
            ("get_stats", NOARGS, writer_get_stats, "get_stats($self, /)\n--\n\nEncoding statistics."),
            ("__enter__", NOARGS, writer_enter),
            ("__exit__", FAST, writer_exit),
        ];
        t.tp_getset = crate::getset_table![
            ("total_samples", writer_total_samples; "Number of thread samples written."),
            ("limit_reached", writer_limit_reached; "True once a sample exceeded a limit of the format."),
        ];
        crate::types::typeobject::type_ready(writer_type())?;

        let t = &mut *reader_type();
        t.tp_name = crate::methods::cstr_static("_remote_debugging.BinaryReader");
        t.tp_basicsize = std::mem::size_of::<Reader>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_new = Some(reader_new);
        t.tp_dealloc = Some(reader_dealloc);
        t.tp_traverse = Some(empty_traverse);
        t.tp_doc = crate::methods::cstr_static("BinaryReader(filename)\n--\n\nRead a profile written by BinaryWriter.");
        t.tp_methods = crate::mdefs![
            ("get_info", NOARGS, reader_get_info, "get_info($self, /)\n--\n\nThe file's metadata."),
            ("replay", FASTKW, reader_replay, "replay($self, /, collector, progress_callback=None)\n--\n\nFeed every sample to collector.collect(); return the sample count."),
            ("close", NOARGS, reader_close, "close($self, /)\n--\n\nRelease the file's data."),
            ("__enter__", NOARGS, reader_enter),
            ("__exit__", FAST, reader_exit),
        ];
        crate::types::typeobject::type_ready(reader_type())?;
    }
    dict::set_item_str(d, "BinaryWriter", &crate::types::typeobject::type_ref(writer_type()))?;
    dict::set_item_str(d, "BinaryReader", &crate::types::typeobject::type_ref(reader_type()))?;
    Ok(())
}
