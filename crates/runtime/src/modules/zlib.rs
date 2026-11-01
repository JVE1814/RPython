//! The `zlib` module on top of the from-scratch DEFLATE implementation in `rpy-compress`.

use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::modules::compress_common::{self as cc, ExcCell};
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, int, str as pystr};
use rpy_compress::checksum;
use rpy_compress::deflate::{Deflater, Flush, Wrap};
use rpy_compress::inflate::{Format, InflateError, Inflater, Status};

pub const ZLIB_VERSION: &str = "1.3.1";
pub const DEF_BUF_SIZE: i64 = 16384;

static ERROR: ExcCell = ExcCell::new();

#[unsafe(no_mangle)]
pub static mut _PyZlib_Compress_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyZlib_Decompress_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyZlib_ZlibDecompressor_Type: PyTypeObject = PyTypeObject::ZERO;

fn comp_type() -> *mut PyTypeObject {
    &raw mut _PyZlib_Compress_Type
}
fn decomp_type() -> *mut PyTypeObject {
    &raw mut _PyZlib_Decompress_Type
}
fn zd_type() -> *mut PyTypeObject {
    &raw mut _PyZlib_ZlibDecompressor_Type
}

fn error(msg: &str) -> PyErr {
    new_err(ERROR.ptr(), msg)
}

/// Format a zlib error like CPython's `zlib_error()`.
fn zerr(code: i32, msg: Option<&str>, what: &str) -> PyErr {
    let zmsg = msg.or(match code {
        -5 => Some("incomplete or truncated stream"),
        -2 => Some("inconsistent stream state"),
        -3 => Some("invalid input data"),
        _ => None,
    });
    match zmsg {
        Some(m) => error(&format!("Error {} {}: {}", code, what, m)),
        None => error(&format!("Error {} {}", code, what)),
    }
}

fn ierr(e: InflateError, what: &str) -> PyErr {
    zerr(e.code, e.msg, what)
}

/// Decoder wbits -> (format, window bits); None if invalid.
fn inflate_params(wbits: i32) -> Option<(Format, u32)> {
    match wbits {
        -15..=-8 => Some((Format::Raw, (-wbits) as u32)),
        0 => Some((Format::Zlib, 0)),
        8..=15 => Some((Format::Zlib, wbits as u32)),
        16 => Some((Format::Gzip, 15)),
        24..=31 => Some((Format::Gzip, (wbits - 16) as u32)),
        32 => Some((Format::Auto, 0)),
        40..=47 => Some((Format::Auto, (wbits - 32) as u32)),
        _ => None,
    }
}

/// Encoder wbits -> (wrapper, window bits); None if invalid.
fn deflate_params(wbits: i32) -> Option<(Wrap, u32)> {
    match wbits {
        -15..=-9 => Some((Wrap::Raw, (-wbits) as u32)),
        8..=15 => Some((Wrap::Zlib, wbits.max(9) as u32)),
        25..=31 => Some((Wrap::Gzip, (wbits - 16) as u32)),
        _ => None,
    }
}

fn opt_int(o: Option<&PyObject>, default: i32) -> PyResult<i32> {
    match o {
        None => Ok(default),
        Some(o) => cc::c_int(o),
    }
}

// ------------------------------------------------------------------ checksums

fn z_adler32(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("adler32", a, 1, 2)?;
    let b = cc::buffer(a[0])?;
    let v = match a.get(1) {
        Some(o) => cc::c_uint_wrap(o)?,
        None => 1,
    };
    let d = b.data();
    let r = cc::maybe_blocking(d.len(), || checksum::adler32(v, d));
    Ok(int::new_int(r as i64))
}

fn z_crc32(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("crc32", a, 1, 2)?;
    let b = cc::buffer(a[0])?;
    let v = match a.get(1) {
        Some(o) => cc::c_uint_wrap(o)?,
        None => 0,
    };
    let d = b.data();
    let r = cc::maybe_blocking(d.len(), || checksum::crc32(v, d));
    Ok(int::new_int(r as i64))
}

fn combine_len(o: &PyObject) -> PyResult<u64> {
    let n = cc::ssize(o)?;
    if n < 0 {
        raise!(ValueError, "len2 must be non-negative");
    }
    Ok(n as u64)
}

fn z_adler32_combine(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("adler32_combine", a, 3, 3)?;
    let x = cc::c_uint_wrap(a[0])?;
    let y = cc::c_uint_wrap(a[1])?;
    let n = combine_len(a[2])?;
    Ok(int::new_int(checksum::adler32_combine(x, y, n) as i64))
}

fn z_crc32_combine(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("crc32_combine", a, 3, 3)?;
    let x = cc::c_uint_wrap(a[0])?;
    let y = cc::c_uint_wrap(a[1])?;
    let n = combine_len(a[2])?;
    Ok(int::new_int(checksum::crc32_combine(x, y, n) as i64))
}

// ------------------------------------------------------------------ one-shot functions

fn z_compress(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("compress", &["data", "level", "wbits"], 1, 1);
    let [data, level, wbits] = args::parse::<3>(&P, a, kw)?;
    let b = cc::buffer(data.unwrap())?;
    let level = opt_int(level, -1)?;
    let wbits = opt_int(wbits, 15)?;
    if !(-1..=9).contains(&level) {
        return Err(error("Bad compression level"));
    }
    let Some((wrap, wb)) = deflate_params(wbits) else {
        return Err(error("Bad compression level"));
    };
    let level = if level < 0 { 6 } else { level as u32 };
    let d = b.data();
    let out = cc::maybe_blocking(d.len(), || {
        let mut c = Deflater::new(level, wrap, wb, 8, 0);
        c.compress(d, Flush::Finish);
        c.take_output()
    });
    cc::new_bytes(&out)
}

fn z_decompress(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("decompress", &["data", "wbits", "bufsize"], 1, 1);
    let [data, wbits, bufsize] = args::parse::<3>(&P, a, kw)?;
    let b = cc::buffer(data.unwrap())?;
    let wbits = opt_int(wbits, 15)?;
    if let Some(bs) = bufsize {
        if cc::ssize(bs)? < 0 {
            raise!(ValueError, "bufsize must be non-negative");
        }
    }
    let Some((fmt, wb)) = inflate_params(wbits) else {
        return Err(zerr(-2, None, "while preparing to decompress data"));
    };
    let d = b.data();
    let r = cc::maybe_blocking(d.len(), || -> Result<Vec<u8>, InflateError> {
        let mut inf = Inflater::new(fmt, wb);
        let (_, st) = inf.inflate(d, usize::MAX)?;
        if st != Status::StreamEnd {
            return Err(InflateError { code: -5, msg: None });
        }
        Ok(inf.output().to_vec())
    });
    match r {
        Ok(v) => cc::new_bytes(&v),
        Err(e) => Err(ierr(e, "while decompressing data")),
    }
}

// ------------------------------------------------------------------ Compress objects

struct CompState {
    d: Option<Deflater>,
}

fn z_compressobj(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("compressobj", &["level", "method", "wbits", "memLevel", "strategy", "zdict"], 0, 0);
    let [level, method, wbits, memlevel, strategy, zdict] = args::parse::<6>(&P, a, kw)?;
    let level = opt_int(level, -1)?;
    let method = opt_int(method, 8)?;
    let wbits = opt_int(wbits, 15)?;
    let memlevel = opt_int(memlevel, 8)?;
    let strategy = opt_int(strategy, 0)?;
    let zd = match zdict {
        Some(z) if !cc::is_none(z) => match cc::buffer(z) {
            Ok(b) => Some(b.data().to_vec()),
            Err(_) => {
                clear_exc();
                raise!(TypeError, "zdict argument must support the buffer protocol");
            }
        },
        _ => None,
    };
    let params = deflate_params(wbits);
    if !(-1..=9).contains(&level) || method != 8 || !(1..=9).contains(&memlevel) || !(0..=4).contains(&strategy) || params.is_none() {
        raise!(ValueError, "Invalid initialization option");
    }
    let (wrap, wb) = params.unwrap();
    let level = if level < 0 { 6 } else { level as u32 };
    let mut d = Deflater::new(level, wrap, wb, memlevel as u32, strategy as u32);
    if let Some(z) = zd {
        d.set_dictionary(&z);
    }
    cc::new_obj(comp_type(), CompState { d: Some(d) })
}

fn comp_compress(o: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("compress", a, 1, 1)?;
    let b = cc::buffer(a[0])?;
    let mut st = cc::lock::<CompState>(o)?;
    let Some(d) = st.d.as_mut() else {
        return Err(zerr(-2, None, "while compressing data"));
    };
    let data = b.data();
    let out = cc::maybe_blocking(data.len(), || {
        d.compress(data, Flush::None);
        d.take_output()
    });
    drop(st);
    cc::new_bytes(&out)
}

fn comp_flush(o: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("flush", a, 0, 1)?;
    let mode = match a.first() {
        Some(m) => cc::c_int(m)?,
        None => 4,
    };
    if mode == 0 {
        return cc::new_bytes(b"");
    }
    let mut st = cc::lock::<CompState>(o)?;
    let Some(d) = st.d.as_mut() else {
        return Err(zerr(-2, None, "while flushing"));
    };
    let Some(f) = Flush::from_int(mode as i64) else {
        return Err(zerr(-2, None, "while flushing"));
    };
    d.compress(&[], f);
    let out = d.take_output();
    if f == Flush::Finish {
        st.d = None;
    }
    drop(st);
    cc::new_bytes(&out)
}

fn comp_copy(o: &PyObject) -> PyResult {
    let st = cc::lock::<CompState>(o)?;
    let Some(d) = st.d.as_ref() else {
        raise!(ValueError, "Inconsistent stream state");
    };
    let n = CompState { d: Some(d.clone()) };
    drop(st);
    cc::new_obj(comp_type(), n)
}

fn comp_deepcopy(o: &PyObject, _memo: &PyObject) -> PyResult {
    comp_copy(o)
}

// ------------------------------------------------------------------ Decompress objects

struct DecompState {
    inf: Inflater,
    unused: Vec<u8>,
    tail: Vec<u8>,
    eof: bool,
    ended: bool,
}

fn zdict_arg(z: Option<&PyObject>) -> PyResult<Option<Vec<u8>>> {
    match z {
        None => Ok(None),
        Some(z) => match cc::buffer(z) {
            Ok(b) => Ok(Some(b.data().to_vec())),
            Err(_) => {
                clear_exc();
                raise!(TypeError, "zdict argument must support the buffer protocol");
            }
        },
    }
}

fn z_decompressobj(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("decompressobj", &["wbits", "zdict"], 0, 0);
    let [wbits, zdict] = args::parse::<2>(&P, a, kw)?;
    let wbits = opt_int(wbits, 15)?;
    let zd = zdict_arg(zdict)?;
    let Some((fmt, wb)) = inflate_params(wbits) else {
        raise!(ValueError, "Invalid initialization option");
    };
    let mut inf = Inflater::new(fmt, wb);
    if let Some(z) = zd {
        inf.set_dictionary(&z);
    }
    cc::new_obj(decomp_type(), DecompState { inf, unused: Vec::new(), tail: Vec::new(), eof: false, ended: false })
}

fn decomp_decompress(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("decompress", &["data", "max_length"], 1, 1);
    let [data, max_length] = args::parse::<2>(&P, a, kw)?;
    let b = cc::buffer(data.unwrap())?;
    let max_length = match max_length {
        Some(m) => cc::ssize(m)?,
        None => 0,
    };
    if max_length < 0 {
        raise!(ValueError, "max_length must be non-negative");
    }
    let limit = if max_length == 0 { usize::MAX } else { max_length as usize };
    let mut st = cc::lock::<DecompState>(o)?;
    let st = &mut *st;
    let d = b.data();
    if st.eof {
        st.unused.extend_from_slice(d);
        st.tail.clear();
        return cc::new_bytes(b"");
    }
    let r = cc::maybe_blocking(d.len().max(if limit == usize::MAX { d.len() } else { limit }), || st.inf.inflate(d, limit));
    let (used, status) = match r {
        Ok(x) => x,
        Err(e) => return Err(ierr(e, "while decompressing data")),
    };
    let out = cc::new_bytes(st.inf.output());
    st.inf.consume_output();
    match status {
        Status::StreamEnd => {
            st.eof = true;
            let u = st.inf.take_unused();
            st.unused.extend_from_slice(&u);
            st.tail.clear();
        }
        Status::OutputFull => {
            st.tail = d[used..].to_vec();
        }
        Status::NeedInput => st.tail.clear(),
    }
    out
}

fn decomp_flush(o: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("flush", a, 0, 1)?;
    if let Some(l) = a.first() {
        if cc::ssize(l)? <= 0 {
            raise!(ValueError, "length must be greater than zero");
        }
    }
    let mut st = cc::lock::<DecompState>(o)?;
    let st = &mut *st;
    if st.eof {
        st.ended = true;
        return cc::new_bytes(b"");
    }
    let tail = std::mem::take(&mut st.tail);
    let r = cc::maybe_blocking(tail.len(), || st.inf.inflate(&tail, usize::MAX));
    let (_, status) = match r {
        Ok(x) => x,
        Err(e) => return Err(ierr(e, "while flushing")),
    };
    let out = cc::new_bytes(st.inf.output());
    st.inf.consume_output();
    if status == Status::StreamEnd {
        st.eof = true;
        st.ended = true;
        let u = st.inf.take_unused();
        st.unused.extend_from_slice(&u);
    }
    out
}

fn decomp_copy(o: &PyObject) -> PyResult {
    let st = cc::lock::<DecompState>(o)?;
    if st.ended {
        raise!(ValueError, "Inconsistent stream state");
    }
    let n = DecompState { inf: st.inf.clone(), unused: st.unused.clone(), tail: st.tail.clone(), eof: st.eof, ended: false };
    drop(st);
    cc::new_obj(decomp_type(), n)
}

fn decomp_deepcopy(o: &PyObject, _memo: &PyObject) -> PyResult {
    decomp_copy(o)
}

fn decomp_unused(o: &PyObject) -> PyResult {
    let st = cc::lock::<DecompState>(o)?;
    cc::new_bytes(&st.unused)
}

fn decomp_tail(o: &PyObject) -> PyResult {
    let st = cc::lock::<DecompState>(o)?;
    cc::new_bytes(&st.tail)
}

fn decomp_eof(o: &PyObject) -> PyResult {
    let st = cc::lock::<DecompState>(o)?;
    Ok(bool_::new_bool(st.eof))
}

// ------------------------------------------------------------------ _ZlibDecompressor

pub struct ZdState {
    pub inf: Inflater,
    pub input: Vec<u8>,
    pub unused: Vec<u8>,
    pub eof: bool,
    pub needs_input: bool,
}

/// Shared decompress() logic for the bz2/lzma-style decompressor objects.
pub fn zd_decompress_impl(st: &mut ZdState, data: &[u8], max_length: isize) -> PyResult {
    if st.eof {
        raise!(EOFError, "End of stream already reached");
    }
    let limit = if max_length < 0 { usize::MAX } else { max_length as usize };
    let use_buf = !st.input.is_empty();
    if use_buf {
        st.input.extend_from_slice(data);
    }
    let inp = std::mem::take(&mut st.input);
    let src: &[u8] = if use_buf { &inp } else { data };
    let r = cc::maybe_blocking(src.len(), || if limit == 0 { Ok((0, Status::OutputFull)) } else { st.inf.inflate(src, limit) });
    let (used, status) = match r {
        Ok(x) => x,
        Err(e) => return Err(ierr(e, "while decompressing data")),
    };
    let out_len = st.inf.output_len();
    let out = cc::new_bytes(st.inf.output());
    st.inf.consume_output();
    if status == Status::StreamEnd {
        st.eof = true;
        st.needs_input = false;
        let u = st.inf.take_unused();
        st.unused.extend_from_slice(&u);
    } else {
        let rest = &src[used..];
        if rest.is_empty() {
            st.needs_input = !(limit != usize::MAX && out_len == limit);
        } else {
            st.input = rest.to_vec();
            st.needs_input = false;
        }
    }
    out
}

unsafe extern "C" fn zd_new(tp: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (v, kn) = args::kwdict_to_vectorcall(&*a, kw);
            let vr: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            static P: Params = Params::new("_ZlibDecompressor", &["wbits", "zdict"], 0, 0);
            let [wbits, zdict] = args::parse::<2>(&P, &vr, kn.as_deref())?;
            let wbits = opt_int(wbits, 15)?;
            let zd = match zdict {
                Some(z) => match cc::buffer(z) {
                    Ok(b) => Some(b.data().to_vec()),
                    Err(e) => return Err(e),
                },
                None => None,
            };
            let Some((fmt, wb)) = inflate_params(wbits) else {
                return Err(zerr(-2, None, "while creating decompression object"));
            };
            let mut inf = Inflater::new(fmt, wb);
            if let Some(z) = zd {
                inf.set_dictionary(&z);
            }
            cc::new_obj(tp, ZdState { inf, input: Vec::new(), unused: Vec::new(), eof: false, needs_input: true })
        })();
        into_c(r)
    }
}

fn zd_decompress(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("decompress", &["data", "max_length"], 0, 1);
    let [data, max_length] = args::parse::<2>(&P, a, kw)?;
    let b = cc::buffer(data.unwrap())?;
    let ml = match max_length {
        Some(m) => cc::ssize(m)?,
        None => -1,
    };
    let mut st = cc::lock::<ZdState>(o)?;
    zd_decompress_impl(&mut st, b.data(), ml)
}

fn zd_eof(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(cc::lock::<ZdState>(o)?.eof))
}
fn zd_unused(o: &PyObject) -> PyResult {
    cc::new_bytes(&cc::lock::<ZdState>(o)?.unused)
}
fn zd_needs_input(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(cc::lock::<ZdState>(o)?.needs_input))
}

// ------------------------------------------------------------------ module

/// Module `__getattr__`: the deprecated `__version__` attribute.
fn z_getattr(_m: &PyObject, name: &PyObject) -> PyResult {
    let n = pystr::to_string_lossy(name);
    if n == "__version__" {
        crate::modules::warnings::warn(
            crate::types::exceptions::DeprecationWarning(),
            "'__version__' is deprecated and slated for removal in Python 3.20",
            1,
        )?;
        return Ok(pystr::new_str("1.0"));
    }
    raise!(AttributeError, "module 'zlib' has no attribute '{}'", n);
}

static INIT: std::sync::Once = std::sync::Once::new();

fn init_types() -> PyResult<()> {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        cc::init_state_type::<CompState>(comp_type(), c"zlib.Compress", Py_TPFLAGS_DISALLOW_INSTANTIATION as u64);
        let t = &mut *comp_type();
        t.tp_methods = crate::mdefs![
            ("compress", FAST, comp_compress, "compress($self, data, /)\n--\n\nReturns a bytes object containing compressed data."),
            ("flush", FAST, comp_flush, "flush($self, mode=zlib.Z_FINISH, /)\n--\n\nReturn a bytes object containing any remaining compressed data."),
            ("copy", NOARGS, comp_copy, "copy($self, /)\n--\n\nReturn a copy of the compression object."),
            ("__copy__", NOARGS, comp_copy),
            ("__deepcopy__", O, comp_deepcopy),
            ("__reduce__", NOARGS, cc::no_pickle),
        ];
        res = type_ready(comp_type());
        (*comp_type()).tp_new = None;
        if res.is_err() {
            return;
        }
        cc::init_state_type::<DecompState>(decomp_type(), c"zlib.Decompress", Py_TPFLAGS_DISALLOW_INSTANTIATION as u64);
        let t = &mut *decomp_type();
        t.tp_methods = crate::mdefs![
            ("decompress", FASTKW, decomp_decompress, "decompress($self, data, /, max_length=0)\n--\n\nReturn a bytes object containing the decompressed version of the data."),
            ("flush", FAST, decomp_flush, "flush($self, length=zlib.DEF_BUF_SIZE, /)\n--\n\nReturn a bytes object containing any remaining decompressed data."),
            ("copy", NOARGS, decomp_copy, "copy($self, /)\n--\n\nReturn a copy of the decompression object."),
            ("__copy__", NOARGS, decomp_copy),
            ("__deepcopy__", O, decomp_deepcopy),
            ("__reduce__", NOARGS, cc::no_pickle),
        ];
        t.tp_getset = crate::getset_table![
            ("unused_data", decomp_unused),
            ("unconsumed_tail", decomp_tail),
            ("eof", decomp_eof),
        ];
        res = type_ready(decomp_type());
        (*decomp_type()).tp_new = None;
        if res.is_err() {
            return;
        }
        cc::init_state_type::<ZdState>(zd_type(), c"zlib._ZlibDecompressor", 0);
        let t = &mut *zd_type();
        t.tp_new = Some(zd_new);
        t.tp_doc = c"_ZlibDecompressor(wbits=15, zdict=b'')\n--\n\nCreate a decompressor object for decompressing data incrementally.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("decompress", FASTKW, zd_decompress, "decompress($self, /, data, max_length=-1)\n--\n\nDecompress *data*, returning uncompressed data as bytes."),
            ("__reduce__", NOARGS, cc::no_pickle),
        ];
        t.tp_getset = crate::getset_table![
            ("eof", zd_eof; "True if the end-of-stream marker has been reached."),
            ("unused_data", zd_unused; "Data found after the end of the compressed stream."),
            ("needs_input", zd_needs_input; "True if more input is needed before more decompressed data can be produced."),
        ];
        res = type_ready(zd_type());
    });
    res
}

pub fn init_module() -> PyResult {
    init_types()?;
    let m = crate::types::module::new_module("zlib")?;
    let d = crate::types::module::module_dict(&m);
    cc::set_str(
        &d,
        "__doc__",
        "The functions in this module allow compression and decompression using the\nzlib library, which is based on GNU zip.\n\nadler32(string[, start]) -- Compute an Adler-32 checksum.\ncompress(data[, level]) -- Compress data, with compression level 0-9 or -1.\ncompressobj([level[, ...]]) -- Return a compressor object.\ncrc32(string[, start]) -- Compute a CRC-32 checksum.\ndecompress(string,[wbits],[bufsize]) -- Decompresses a compressed string.\ndecompressobj([wbits[, zdict]]) -- Return a decompressor object.\n\n'wbits' is window buffer size and container format.\nCompressor objects support compress() and flush() methods; decompressor\nobjects support decompress() and flush().",
    )?;
    let err = ERROR.get_or_create("zlib", "error", crate::types::exceptions::Exception())?;
    dict::set_item_str(&d, "error", &type_ref(err))?;
    dict::set_item_str(&d, "_ZlibDecompressor", &type_ref(zd_type()))?;
    for (k, v) in [
        ("MAX_WBITS", 15i64),
        ("DEFLATED", 8),
        ("DEF_MEM_LEVEL", 8),
        ("DEF_BUF_SIZE", DEF_BUF_SIZE),
        ("Z_NO_COMPRESSION", 0),
        ("Z_BEST_SPEED", 1),
        ("Z_BEST_COMPRESSION", 9),
        ("Z_DEFAULT_COMPRESSION", -1),
        ("Z_FILTERED", 1),
        ("Z_HUFFMAN_ONLY", 2),
        ("Z_RLE", 3),
        ("Z_FIXED", 4),
        ("Z_DEFAULT_STRATEGY", 0),
        ("Z_NO_FLUSH", 0),
        ("Z_PARTIAL_FLUSH", 1),
        ("Z_SYNC_FLUSH", 2),
        ("Z_FULL_FLUSH", 3),
        ("Z_FINISH", 4),
        ("Z_BLOCK", 5),
        ("Z_TREES", 6),
    ] {
        cc::set_int(&d, k, v)?;
    }
    cc::set_str(&d, "ZLIB_VERSION", ZLIB_VERSION)?;
    cc::set_str(&d, "ZLIB_RUNTIME_VERSION", ZLIB_VERSION)?;
    let methods = crate::mdefs![
        ("__getattr__", O, z_getattr),
        ("adler32", FAST, z_adler32, "adler32($module, data, value=1, /)\n--\n\nCompute an Adler-32 checksum of data."),
        ("crc32", FAST, z_crc32, "crc32($module, data, value=0, /)\n--\n\nCompute a CRC-32 checksum of data."),
        ("adler32_combine", FAST, z_adler32_combine, "adler32_combine($module, adler1, adler2, len2, /)\n--\n\nCombine two Adler-32 check values into one."),
        ("crc32_combine", FAST, z_crc32_combine, "crc32_combine($module, crc1, crc2, len2, /)\n--\n\nCombine two CRC-32 check values into one."),
        ("compress", FASTKW, z_compress, "compress($module, data, /, level=Z_DEFAULT_COMPRESSION, wbits=MAX_WBITS)\n--\n\nReturns a bytes object containing compressed data."),
        ("decompress", FASTKW, z_decompress, "decompress($module, data, /, wbits=MAX_WBITS, bufsize=DEF_BUF_SIZE)\n--\n\nReturns a bytes object containing the uncompressed data."),
        ("compressobj", FASTKW, z_compressobj, "compressobj($module, /, level=Z_DEFAULT_COMPRESSION, method=DEFLATED,\n            wbits=MAX_WBITS, memLevel=DEF_MEM_LEVEL,\n            strategy=Z_DEFAULT_STRATEGY, zdict=None)\n--\n\nReturn a compressor object."),
        ("decompressobj", FASTKW, z_decompressobj, "decompressobj($module, /, wbits=MAX_WBITS, zdict=b\'\')\n--\n\nReturn a decompressor object."),
    ];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    let _ = pystr::new_str;
    Ok(m)
}
