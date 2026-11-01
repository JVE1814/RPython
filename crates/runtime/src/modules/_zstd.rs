//! `_zstd`: ZstdCompressor / ZstdDecompressor / ZstdDict and the frame and dictionary
//! helpers used by `compression.zstd`, backed by `rpy_compress::zstd`.

use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::modules::compress_common::{self as cc, ExcCell};
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, bytes, dict, int, str as pystr, tuple};
use rpy_compress::zstd::{self as z, dec, enc};
use std::sync::{Arc, Mutex};

static ERROR: ExcCell = ExcCell::new();

#[unsafe(no_mangle)]
pub static mut _PyZstd_Compressor_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyZstd_Decompressor_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyZstd_Dict_Type: PyTypeObject = PyTypeObject::ZERO;

fn ctype() -> *mut PyTypeObject {
    &raw mut _PyZstd_Compressor_Type
}
fn dtype() -> *mut PyTypeObject {
    &raw mut _PyZstd_Decompressor_Type
}
fn ztype() -> *mut PyTypeObject {
    &raw mut _PyZstd_Dict_Type
}

fn zerr(msg: &str) -> PyErr {
    new_err(ERROR.ptr(), msg)
}

const CONTINUE: i32 = 0;
const FLUSH_BLOCK: i32 = 1;
const FLUSH_FRAME: i32 = 2;

const DICT_DIGESTED: i64 = 0;
const DICT_UNDIGESTED: i64 = 1;
const DICT_PREFIX: i64 = 2;

// ------------------------------------------------------------------ parameters

/// (key, name, low, high) of every compression parameter.
const CPARAMS: [(i32, &str, i32, i32); 19] = [
    (100, "compression_level", z::CLEVEL_MIN, z::CLEVEL_MAX),
    (101, "window_log", 10, 31),
    (102, "hash_log", 6, 30),
    (103, "chain_log", 6, 30),
    (104, "search_log", 1, 30),
    (105, "min_match", 3, 7),
    (106, "target_length", 0, 131072),
    (107, "strategy", 1, 9),
    (160, "enable_long_distance_matching", 0, 2),
    (161, "ldm_hash_log", 6, 30),
    (162, "ldm_min_match", 4, 4096),
    (163, "ldm_bucket_size_log", 1, 8),
    (164, "ldm_hash_rate_log", 0, 25),
    (200, "content_size_flag", 0, 1),
    (201, "checksum_flag", 0, 1),
    (202, "dict_id_flag", 0, 1),
    (400, "nb_workers", 0, 0),
    (401, "job_size", 0, 0),
    (402, "overlap_log", 0, 0),
];
const DPARAMS: [(i32, &str, i32, i32); 1] = [(100, "window_log_max", 10, 31)];

/// The enum classes registered by `compression.zstd` (to reject the wrong kind of key).
static PARAM_TYPES: Mutex<Option<(usize, usize)>> = Mutex::new(None);

fn param_types() -> Option<(PyRef, PyRef)> {
    let g = PARAM_TYPES.lock().unwrap();
    g.map(|(c, d)| unsafe { (PyRef::from_borrowed(c as *mut PyObject), PyRef::from_borrowed(d as *mut PyObject)) })
}

/// Validate an options dict: returns (key, value) pairs.
fn parse_options(o: &PyObject, compress: bool) -> PyResult<Vec<(i32, i32)>> {
    if !dict::is_dict(o) {
        raise!(TypeError, "options argument should be a dict object");
    }
    let wrong = param_types().map(|(c, d)| if compress { d } else { c });
    let items_obj = ab::call_method_str(o, "items", &[])?;
    let items = ab::collect_iterable(&items_obj)?;
    let (table, what): (&[(i32, &str, i32, i32)], &str) = if compress { (&CPARAMS, "compression") } else { (&DPARAMS, "decompression") };
    let mut out = Vec::new();
    for it in items {
        let k = tuple::tuple_get(&it, 0);
        let v = tuple::tuple_get(&it, 1);
        if let Some(w) = &wrong {
            if ab::isinstance(k, w)? {
                let other = if compress { "DecompressionParameter" } else { "CompressionParameter" };
                let mine = if compress { "CompressionParameter" } else { "DecompressionParameter" };
                raise!(TypeError, "{} options key must not be a {} attribute (use {})", what, other, mine);
            }
        }
        if !int::is_int(k) {
            raise!(TypeError, "{} options keys must be int objects, not {}", what, tname(k));
        }
        if !int::is_int(v) {
            raise!(TypeError, "{} options values must be int objects, not {}", what, tname(v));
        }
        let key = cc::c_int(k)?;
        let val = cc::c_int(v)?;
        match table.iter().find(|p| p.0 == key) {
            None => raise!(ValueError, "invalid {} parameter 'unknown parameter (key {})'", what, key),
            Some(&(_, name, lo, hi)) => {
                if val < lo || val > hi {
                    raise!(ValueError, "{} parameter '{}' received an illegal value {}; the valid range is [{}, {}]", what, name, val, lo, hi);
                }
            }
        }
        out.push((key, val));
    }
    Ok(out)
}

fn level_error(v: Option<i64>) -> PyErr {
    match v {
        Some(v) => err!(ValueError, "illegal compression level {}; the valid range is [{}, {}]", v, z::CLEVEL_MIN, z::CLEVEL_MAX),
        None => err!(ValueError, "illegal compression level; the valid range is [{}, {}]", z::CLEVEL_MIN, z::CLEVEL_MAX),
    }
}

fn parse_level(o: &PyObject) -> PyResult<i32> {
    if !int::is_int(o) {
        raise!(TypeError, "compression level must be an int, not {}", tname(o));
    }
    match int::as_i64(o) {
        Some(v) if v >= z::CLEVEL_MIN as i64 && v <= z::CLEVEL_MAX as i64 => Ok(v as i32),
        Some(v) if v >= i32::MIN as i64 && v <= i32::MAX as i64 => Err(level_error(Some(v))),
        _ => Err(level_error(None)),
    }
}

// ------------------------------------------------------------------ ZstdDict

struct ZDict {
    content: PyRef,
    id: u32,
    cdict: Option<Arc<enc::CDict>>,
    ddict: Option<Arc<dec::DDict>>,
}

fn dict_bytes(o: &PyObject) -> &[u8] {
    bytes::bytes_data(o)
}

unsafe extern "C" fn zdict_new(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (v, kn) = args::kwdict_to_vectorcall(&*a, kw);
            let vr: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            static P: Params = Params::new("ZstdDict", &["dict_content", "is_raw"], 1, 1).kwonly(1);
            let [content, is_raw] = args::parse::<2>(&P, &vr, kn.as_deref())?;
            let content = content.unwrap();
            let buf = cc::buffer(content)?;
            let data = buf.data().to_vec();
            let is_raw = match is_raw {
                Some(o) => ab::is_true(o)?,
                None => false,
            };
            if data.len() < 8 {
                raise!(ValueError, "Zstandard dictionary content must be at least 8 bytes long");
            }
            let id = if u32::from_le_bytes(data[..4].try_into().unwrap()) == z::DICT_MAGIC {
                u32::from_le_bytes(data[4..8].try_into().unwrap())
            } else {
                0
            };
            if !is_raw && id == 0 {
                raise!(ValueError, "not a valid Zstandard dictionary (pass is_raw=True to use arbitrary content)");
            }
            let content = cc::new_bytes(&data)?;
            cc::new_obj(t, ZDict { content, id, cdict: None, ddict: None })
        })();
        into_c(r)
    }
}

fn zd_content(o: &PyObject) -> PyResult {
    Ok(cc::lock::<ZDict>(o)?.content.clone())
}
fn zd_id(o: &PyObject) -> PyResult {
    Ok(int::new_int(cc::lock::<ZDict>(o)?.id as i64))
}
fn zd_pair(o: &PyObject, mode: i64) -> PyResult {
    Ok(tuple::new_tuple(vec![o.to_ref(), int::new_int(mode)]))
}
fn zd_digested(o: &PyObject) -> PyResult {
    zd_pair(o, DICT_DIGESTED)
}
fn zd_undigested(o: &PyObject) -> PyResult {
    zd_pair(o, DICT_UNDIGESTED)
}
fn zd_prefix(o: &PyObject) -> PyResult {
    zd_pair(o, DICT_PREFIX)
}

unsafe extern "C" fn zd_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        match cc::lock::<ZDict>(&*o) {
            Ok(g) => dict_bytes(&g.content).len() as Py_ssize_t,
            Err(_) => -1,
        }
    }
}

unsafe extern "C" fn zd_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let g = cc::lock::<ZDict>(&*o)?;
            Ok(pystr::new_str(&format!("<ZstdDict dict_id={} dict_size={}>", g.id, dict_bytes(&g.content).len())))
        })())
    }
}

fn zd_reduce(o: &PyObject) -> PyResult {
    raise!(TypeError, "cannot pickle '{}' object", tname(o));
}

/// Resolve a `zstd_dict` argument: (dictionary object, mode).
fn dict_arg(o: &PyObject) -> PyResult<(PyRef, i64)> {
    let msg = "zstd_dict argument should be a ZstdDict object, or a (ZstdDict, mode) tuple as returned by its as_* attributes";
    if ab::isinstance(o, type_obj(ztype()))? {
        return Ok((o.to_ref(), DICT_DIGESTED));
    }
    if tuple::is_tuple(o) && tuple::tuple_len(o) == 2 {
        let d = tuple::tuple_get(o, 0);
        let m = tuple::tuple_get(o, 1);
        if ab::isinstance(d, type_obj(ztype()))? && int::is_int(m) {
            let mode = match int::as_i64(m) {
                Some(v) if v >= i32::MIN as i64 && v <= i32::MAX as i64 => v,
                _ => raise!(OverflowError, "zstd_dict mode does not fit in a C int"),
            };
            if (DICT_DIGESTED..=DICT_PREFIX).contains(&mode) {
                return Ok((d.to_ref(), mode));
            }
        }
    }
    raise!(TypeError, "{}", msg);
}

fn load_cdict(d: &PyObject, mode: i64) -> PyResult<Arc<enc::CDict>> {
    let mut g = cc::lock::<ZDict>(d)?;
    let data = dict_bytes(&g.content).to_vec();
    if mode == DICT_PREFIX {
        return Ok(Arc::new(enc::CDict { id: 0, content: data, reps: [1, 4, 8] }));
    }
    if let Some(c) = &g.cdict {
        return Ok(c.clone());
    }
    match enc::CDict::load(&data, false) {
        Ok(c) => {
            let c = Arc::new(c);
            g.cdict = Some(c.clone());
            Ok(c)
        }
        Err(_) => Err(zerr("Unable to build a ZSTD_CDict from the Zstandard dictionary content.")),
    }
}

fn load_ddict(d: &PyObject, mode: i64) -> PyResult<Arc<dec::DDict>> {
    let mut g = cc::lock::<ZDict>(d)?;
    let data = dict_bytes(&g.content).to_vec();
    if mode == DICT_PREFIX {
        return Ok(Arc::new(dec::DDict { id: 0, content: data, entropy: None }));
    }
    if let Some(c) = &g.ddict {
        return Ok(c.clone());
    }
    match dec::DDict::load(&data, false) {
        Ok(c) => {
            let c = Arc::new(c);
            g.ddict = Some(c.clone());
            Ok(c)
        }
        Err(_) => Err(zerr("Unable to build a ZSTD_DDict from the Zstandard dictionary content.")),
    }
}

// ------------------------------------------------------------------ compressor

struct Comp {
    enc: enc::Encoder,
    last_mode: i32,
}

unsafe extern "C" fn comp_new(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (v, kn) = args::kwdict_to_vectorcall(&*a, kw);
            let vr: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            static P: Params = Params::new("ZstdCompressor", &["level", "options", "zstd_dict"], 0, 0);
            let [level, options, zd] = args::parse::<3>(&P, &vr, kn.as_deref())?;
            let level = level.filter(|o| !cc::is_none(o));
            let options = options.filter(|o| !cc::is_none(o));
            let zd = zd.filter(|o| !cc::is_none(o));
            if level.is_some() && options.is_some() {
                raise!(TypeError, "only one of level or options may be given");
            }
            let mut p = enc::Params::from_level(z::CLEVEL_DEFAULT);
            if let Some(l) = level {
                let l = parse_level(l)?;
                p = enc::Params::from_level(l);
            }
            if let Some(o) = options {
                let opts = parse_options(o, true)?;
                if let Some(&(_, l)) = opts.iter().find(|x| x.0 == 100) {
                    p = enc::Params::from_level(l);
                }
                for (k, v) in opts {
                    match k {
                        101 => p.window_log = v as u32,
                        102 => p.hash_log = v as u32,
                        103 => p.chain_log = v as u32,
                        104 => p.search_log = v as u32,
                        105 => p.min_match = v as u32,
                        106 => p.target_length = v as u32,
                        107 => p.strategy = v as u32,
                        200 => p.content_size = v != 0,
                        201 => p.checksum = v != 0,
                        202 => p.dict_id = v != 0,
                        _ => {}
                    }
                }
            }
            let dict = match zd {
                Some(o) => {
                    let (d, mode) = dict_arg(o)?;
                    Some((load_cdict(&d, mode)?, mode == DICT_PREFIX))
                }
                None => None,
            };
            cc::new_obj(t, Comp { enc: enc::Encoder::new(p, dict), last_mode: FLUSH_FRAME })
        })();
        into_c(r)
    }
}

fn run_compress(o: &PyObject, data: &[u8], mode: i32) -> PyResult {
    let mut st = cc::lock::<Comp>(o)?;
    let m = match mode {
        CONTINUE => enc::Mode::Continue,
        FLUSH_BLOCK => enc::Mode::Flush,
        _ => enc::Mode::End,
    };
    let r = cc::maybe_blocking(data.len(), || {
        let mut out = Vec::new();
        st.enc.compress(data, m, &mut out).map(|_| out)
    });
    st.last_mode = mode;
    match r {
        Ok(out) => cc::new_bytes(&out),
        Err(e) => {
            // Start over with a fresh frame after a failure.
            st.last_mode = FLUSH_FRAME;
            let p = st.enc.p;
            st.enc = enc::Encoder::new(p, None);
            Err(zerr(&format!("Zstandard compression failed: {}", e.message())))
        }
    }
}

fn c_compress(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("compress", &["data", "mode"], 0, 1);
    let [data, mode] = args::parse::<2>(&P, a, kw)?;
    let b = cc::buffer(data.unwrap())?;
    let mode = match mode {
        Some(m) => cc::c_int(m)?,
        None => CONTINUE,
    };
    if !(CONTINUE..=FLUSH_FRAME).contains(&mode) {
        raise!(ValueError, "mode must be ZstdCompressor.CONTINUE, ZstdCompressor.FLUSH_BLOCK or ZstdCompressor.FLUSH_FRAME");
    }
    run_compress(o, b.data(), mode)
}

fn c_flush(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("flush", &["mode"], 0, 0);
    let [mode] = args::parse::<1>(&P, a, kw)?;
    let mode = match mode {
        Some(m) => cc::c_int(m)?,
        None => FLUSH_FRAME,
    };
    if mode != FLUSH_BLOCK && mode != FLUSH_FRAME {
        raise!(ValueError, "mode must be ZstdCompressor.FLUSH_BLOCK or ZstdCompressor.FLUSH_FRAME");
    }
    run_compress(o, &[], mode)
}

fn c_set_pledged(o: &PyObject, size: &PyObject) -> PyResult {
    let max = u64::MAX - 1;
    let v = if cc::is_none(size) {
        None
    } else {
        let bad = || err!(ValueError, "size argument should be a positive int less than {}", max);
        if !int::is_int(size) {
            raise!(TypeError, "size must be an int or None");
        }
        if int::sign(size) < 0 {
            return Err(bad());
        }
        match rpy_bigint::conv::to_u64(crate::types::int_methods::iref(size)) {
            Some(v) if v < max => Some(v),
            _ => return Err(bad()),
        }
    };
    let mut st = cc::lock::<Comp>(o)?;
    if st.last_mode != FLUSH_FRAME {
        raise!(ValueError, "set_pledged_input_size() method must be called when last_mode == FLUSH_FRAME");
    }
    st.enc.pledged = v;
    Ok(cc::none())
}

fn c_last_mode(o: &PyObject) -> PyResult {
    Ok(int::new_int(cc::lock::<Comp>(o)?.last_mode as i64))
}

// ------------------------------------------------------------------ decompressor

struct Decomp {
    dec: dec::Decoder,
    eof: bool,
    needs_input: bool,
    unused: Option<PyRef>,
}

unsafe extern "C" fn decomp_new(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (v, kn) = args::kwdict_to_vectorcall(&*a, kw);
            let vr: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            static P: Params = Params::new("ZstdDecompressor", &["zstd_dict", "options"], 0, 0);
            let [zd, options] = args::parse::<2>(&P, &vr, kn.as_deref())?;
            let mut wlog = 27u32;
            if let Some(o) = options.filter(|o| !cc::is_none(o)) {
                for (k, v) in parse_options(o, false)? {
                    if k == 100 {
                        wlog = v as u32;
                    }
                }
            }
            let dict = match zd.filter(|o| !cc::is_none(o)) {
                Some(o) => {
                    let (d, mode) = dict_arg(o)?;
                    Some(load_ddict(&d, mode)?)
                }
                None => None,
            };
            cc::new_obj(t, Decomp { dec: dec::Decoder::new(dict, wlog), eof: false, needs_input: true, unused: None })
        })();
        into_c(r)
    }
}

fn d_decompress(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("decompress", &["data", "max_length"], 0, 1);
    let [data, ml] = args::parse::<2>(&P, a, kw)?;
    let b = cc::buffer(data.unwrap())?;
    let ml = match ml {
        Some(m) => cc::ssize(m)?,
        None => -1,
    };
    let mut st = cc::lock::<Decomp>(o)?;
    let st = &mut *st;
    if st.eof {
        raise!(EOFError, "Already at the end of a Zstandard frame.");
    }
    let limit = if ml < 0 { usize::MAX } else { ml as usize };
    let d = b.data();
    let r = cc::maybe_blocking(d.len().max(64 * 1024), || {
        st.dec.feed(d);
        st.dec.read(limit)
    });
    let out = match r {
        Ok(v) => v,
        Err(e) => {
            st.needs_input = false;
            return Err(zerr(&format!("Zstandard decompression failed: {}", e.message())));
        }
    };
    if st.dec.is_done() {
        st.eof = true;
        st.needs_input = false;
        st.unused = Some(cc::new_bytes(st.dec.unused())?);
    } else {
        st.needs_input = !(limit != usize::MAX && out.len() == limit);
    }
    cc::new_bytes(&out)
}

fn d_eof(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(cc::lock::<Decomp>(o)?.eof))
}
fn d_needs_input(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(cc::lock::<Decomp>(o)?.needs_input))
}
fn d_unused(o: &PyObject) -> PyResult {
    let st = cc::lock::<Decomp>(o)?;
    match &st.unused {
        Some(u) => Ok(u.clone()),
        None => cc::new_bytes(&[]),
    }
}

// ------------------------------------------------------------------ module functions

fn frame_buffer(o: &PyObject) -> PyResult<Vec<u8>> {
    Ok(cc::buffer(o)?.data().to_vec())
}

fn m_get_frame_info(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("get_frame_info", &["frame_buffer"], 0, 1);
    let [b] = args::parse::<1>(&P, a, kw)?;
    let d = frame_buffer(b.unwrap())?;
    let bad = || {
        zerr("Error when getting information from the header of a Zstandard frame: make sure the frame_buffer argument starts at the beginning of a frame and is not less than the frame header (6-18 bytes).")
    };
    match dec::parse_frame_header(&d) {
        Ok(Some((h, skippable))) => {
            let size = if skippable { int::new_int(0) } else { h.content_size.map_or_else(cc::none, |c| int::new_int_u128(c as u128)) };
            Ok(tuple::new_tuple(vec![size, int::new_int(h.dict_id as i64)]))
        }
        _ => Err(bad()),
    }
}

fn m_get_frame_size(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("get_frame_size", &["frame_buffer"], 0, 1);
    let [b] = args::parse::<1>(&P, a, kw)?;
    let d = frame_buffer(b.unwrap())?;
    match dec::frame_size(&d) {
        Ok(Some(n)) => Ok(int::new_int(n as i64)),
        _ => Err(zerr("Error when finding the compressed size of a Zstandard frame: make sure the frame_buffer argument starts at the beginning of a frame and is not less than this complete frame.")),
    }
}

/// Validate sample buffers: (data, sizes).
fn samples_args(data: &PyObject, sizes: &PyObject) -> PyResult<(Vec<u8>, Vec<usize>)> {
    if !bytes::is_bytes(data) {
        raise!(TypeError, "samples_bytes must be a bytes object, not {}", tname(data));
    }
    if !tuple::is_tuple(sizes) {
        raise!(TypeError, "samples_sizes must be a tuple, not {}", tname(sizes));
    }
    let d = bytes::bytes_data(data).to_vec();
    let mut v = Vec::new();
    let mut total: u128 = 0;
    for it in tuple::tuple_items(sizes) {
        if !int::is_int(it) {
            raise!(TypeError, "items of samples_sizes must be int objects, not {}", tname(it));
        }
        let n = match int::as_i64(it) {
            Some(n) if n >= 0 => n as usize,
            _ => raise!(ValueError, "items of samples_sizes must be ints between 0 and {}", usize::MAX),
        };
        total += n as u128;
        v.push(n);
    }
    if total != d.len() as u128 {
        raise!(ValueError, "The samples size tuple doesn't match the concatenation's size.");
    }
    Ok((d, v))
}

fn dict_size_arg(o: &PyObject) -> PyResult<usize> {
    if !int::is_int(o) {
        raise!(TypeError, "dict_size must be an int, not {}", tname(o));
    }
    let n = cc::ssize(o)?;
    if n <= 0 {
        raise!(ValueError, "dict_size argument should be positive number.");
    }
    Ok(n as usize)
}

fn m_train_dict(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("train_dict", a, 3, 3)?;
    if !int::is_int(a[2]) {
        raise!(TypeError, "dict_size must be an int, not {}", tname(a[2]));
    }
    let (d, sizes) = samples_args(a[0], a[1])?;
    let size = dict_size_arg(a[2])?;
    let r = crate::threads::blocking(|| z::dict::train(&d, &sizes, size));
    match r {
        Ok(v) => cc::new_bytes(&v),
        Err(e) => Err(zerr(&format!("Unable to train the Zstandard dictionary: {}", e.message()))),
    }
}

fn m_finalize_dict(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("finalize_dict", a, 5, 5)?;
    if !bytes::is_bytes(a[0]) {
        raise!(TypeError, "custom_dict_bytes must be a bytes object, not {}", tname(a[0]));
    }
    for (i, nm) in [(3usize, "dict_size"), (4, "compression_level")] {
        if !int::is_int(a[i]) {
            raise!(TypeError, "{} must be an int, not {}", nm, tname(a[i]));
        }
    }
    let content = bytes::bytes_data(a[0]).to_vec();
    let (d, sizes) = samples_args(a[1], a[2])?;
    let size = dict_size_arg(a[3])?;
    let level = cc::c_int(a[4])?;
    let r = crate::threads::blocking(|| z::dict::finalize(&content, &d, &sizes, size, level));
    match r {
        Ok(v) => cc::new_bytes(&v),
        Err(e) => Err(zerr(&format!("Unable to finalize the Zstandard dictionary: {}", e.message()))),
    }
}

fn m_get_param_bounds(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("get_param_bounds", &["parameter", "is_compress"], 0, 2);
    let [p, c] = args::parse::<2>(&P, a, kw)?;
    let key = cc::c_int(p.unwrap())?;
    let compress = ab::is_true(c.unwrap())?;
    let table: &[(i32, &str, i32, i32)] = if compress { &CPARAMS } else { &DPARAMS };
    match table.iter().find(|x| x.0 == key) {
        Some(&(_, _, lo, hi)) => Ok(tuple::new_tuple(vec![int::new_int(lo as i64), int::new_int(hi as i64)])),
        None => Err(zerr("invalid parameter")),
    }
}

fn m_set_parameter_types(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("set_parameter_types", &["c_parameter_type", "d_parameter_type"], 0, 2);
    let [c, d] = args::parse::<2>(&P, a, kw)?;
    let (c, d) = (c.unwrap(), d.unwrap());
    if !crate::types::typeobject::is_type(c) || !crate::types::typeobject::is_type(d) {
        raise!(ValueError, "the parameter types must be classes");
    }
    let c = c.to_ref().into_raw() as usize;
    let d = d.to_ref().into_raw() as usize;
    *PARAM_TYPES.lock().unwrap() = Some((c, d));
    Ok(cc::none())
}

static INIT: std::sync::Once = std::sync::Once::new();

pub fn init_module() -> PyResult {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        cc::init_state_type::<Comp>(ctype(), c"_zstd.ZstdCompressor", Py_TPFLAGS_BASETYPE as u64);
        let t = &mut *ctype();
        t.tp_new = Some(comp_new);
        t.tp_doc = c"ZstdCompressor(level=None, options=None, zstd_dict=None)\n--\n\nIncremental Zstandard compressor.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("compress", FASTKW, c_compress, "compress($self, /, data, mode=ZstdCompressor.CONTINUE)\n--\n\nFeed data; returns the compressed output that is ready for the given mode."),
            ("flush", FASTKW, c_flush, "flush($self, /, mode=ZstdCompressor.FLUSH_FRAME)\n--\n\nFlush buffered data (ending the frame by default)."),
            ("set_pledged_input_size", O, c_set_pledged, "set_pledged_input_size($self, size, /)\n--\n\nDeclare the uncompressed size of the next frame (None = unknown)."),
            ("__reduce__", NOARGS, cc::no_pickle),
        ];
        t.tp_getset = crate::getset_table![("last_mode", c_last_mode; "Mode of the most recent compress()/flush() call.")];
        res = type_ready(ctype());
        if res.is_err() {
            return;
        }
        for (k, v) in [("CONTINUE", CONTINUE), ("FLUSH_BLOCK", FLUSH_BLOCK), ("FLUSH_FRAME", FLUSH_FRAME)] {
            type_dict_set(ctype(), k, &int::new_int(v as i64));
        }
        cc::init_state_type::<Decomp>(dtype(), c"_zstd.ZstdDecompressor", Py_TPFLAGS_BASETYPE as u64);
        let t = &mut *dtype();
        t.tp_new = Some(decomp_new);
        t.tp_doc = c"ZstdDecompressor(zstd_dict=None, options=None)\n--\n\nIncremental decompressor for one Zstandard frame.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("decompress", FASTKW, d_decompress, "decompress($self, /, data, max_length=-1)\n--\n\nDecode data (at most max_length output bytes when max_length >= 0)."),
            ("__reduce__", NOARGS, cc::no_pickle),
        ];
        t.tp_getset = crate::getset_table![
            ("eof", d_eof; "Whether the end of the frame was reached."),
            ("needs_input", d_needs_input; "Whether decompress() must be given more input to make progress."),
            ("unused_data", d_unused; "Bytes that followed the end of the frame."),
        ];
        res = type_ready(dtype());
        if res.is_err() {
            return;
        }
        cc::init_state_type::<ZDict>(ztype(), c"_zstd.ZstdDict", 0);
        let t = &mut *ztype();
        t.tp_new = Some(zdict_new);
        t.tp_repr = Some(zd_repr);
        t.tp_doc = c"ZstdDict(dict_content, /, *, is_raw=False)\n--\n\nA Zstandard dictionary.".as_ptr();
        let mut sq: PySequenceMethods = std::mem::zeroed();
        sq.sq_length = Some(zd_len);
        t.tp_as_sequence = Box::leak(Box::new(sq));
        t.tp_methods = crate::mdefs![("__reduce__", NOARGS, zd_reduce)];
        t.tp_getset = crate::getset_table![
            ("dict_content", zd_content; "The dictionary bytes."),
            ("dict_id", zd_id; "Dictionary id (0 for raw content)."),
            ("as_digested_dict", zd_digested; "Use the dictionary pre-processed (fast to reuse)."),
            ("as_undigested_dict", zd_undigested; "Use the dictionary loaded per frame."),
            ("as_prefix", zd_prefix; "Use the content as a one-frame prefix."),
        ];
        res = type_ready(ztype());
    });
    res?;
    let m = crate::types::module::new_module("_zstd")?;
    let d = crate::types::module::module_dict(&m);
    let err = ERROR.get_or_create("_zstd", "ZstdError", crate::types::exceptions::Exception())?;
    dict::set_item_str(&d, "ZstdError", &type_ref(err))?;
    dict::set_item_str(&d, "ZstdCompressor", &type_ref(ctype()))?;
    dict::set_item_str(&d, "ZstdDecompressor", &type_ref(dtype()))?;
    dict::set_item_str(&d, "ZstdDict", &type_ref(ztype()))?;
    let (a, b, c) = z::VERSION;
    cc::set_str(&d, "zstd_version", &format!("{}.{}.{}", a, b, c))?;
    cc::set_int(&d, "zstd_version_number", (a * 10000 + b * 100 + c) as i64)?;
    cc::set_int(&d, "ZSTD_CLEVEL_DEFAULT", z::CLEVEL_DEFAULT as i64)?;
    cc::set_int(&d, "ZSTD_DStreamOutSize", (1 << 17) as i64)?;
    for (k, name, _, _) in CPARAMS {
        cc::set_int(&d, &format!("ZSTD_c_{}", name), k as i64)?;
    }
    cc::set_int(&d, "ZSTD_d_window_log_max", 100)?;
    for (i, s) in ["fast", "dfast", "greedy", "lazy", "lazy2", "btlazy2", "btopt", "btultra", "btultra2"].iter().enumerate() {
        cc::set_int(&d, &format!("ZSTD_{}", s), i as i64 + 1)?;
    }
    let methods = crate::mdefs![
        ("get_frame_info", FASTKW, m_get_frame_info, "get_frame_info($module, /, frame_buffer)\n--\n\nReturn (decompressed size or None, dictionary id) from a frame header."),
        ("get_frame_size", FASTKW, m_get_frame_size, "get_frame_size($module, /, frame_buffer)\n--\n\nReturn the compressed size of the frame at the start of the buffer."),
        ("train_dict", FAST, m_train_dict, "train_dict($module, samples_bytes, samples_sizes, dict_size, /)\n--\n\nBuild dictionary bytes from concatenated samples."),
        ("finalize_dict", FAST, m_finalize_dict, "finalize_dict($module, custom_dict_bytes, samples_bytes, samples_sizes, dict_size, compression_level, /)\n--\n\nAdd entropy tables fitted to the samples to custom dictionary content."),
        ("get_param_bounds", FASTKW, m_get_param_bounds, "get_param_bounds($module, /, parameter, is_compress)\n--\n\nReturn the (lower, upper) bounds of a parameter."),
        ("set_parameter_types", FASTKW, m_set_parameter_types, "set_parameter_types($module, /, c_parameter_type, d_parameter_type)\n--\n\nRegister the parameter enum classes."),
    ];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    Ok(m)
}
