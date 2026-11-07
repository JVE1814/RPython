//! `_lzma`: LZMACompressor / LZMADecompressor and the filter-property helpers, backed by the
//! LZMA/XZ codec in `rpy_compress::lzma`.

use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::modules::compress_common::{self as cc, ExcCell};
use crate::object::*;
use crate::{name, raise};
use crate::types::typeobject::*;
use crate::types::{bool_, dict, int, str as pystr};
use rpy_compress::lzma::filters::*;
use rpy_compress::lzma::{self as lz, Decoder, Encoder, FilterSpec, Format, LzOptions};

static ERROR: ExcCell = ExcCell::new();

#[unsafe(no_mangle)]
pub static mut _PyLZMA_Compressor_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyLZMA_Decompressor_Type: PyTypeObject = PyTypeObject::ZERO;

fn ctype() -> *mut PyTypeObject {
    &raw mut _PyLZMA_Compressor_Type
}
fn dtype() -> *mut PyTypeObject {
    &raw mut _PyLZMA_Decompressor_Type
}

const FORMAT_AUTO: i32 = 0;
const FORMAT_XZ: i32 = 1;
const FORMAT_ALONE: i32 = 2;
const FORMAT_RAW: i32 = 3;

fn lzma_error(msg: &str) -> PyErr {
    new_err(ERROR.ptr(), msg)
}

fn codec_err(e: lz::Error) -> PyErr {
    lzma_error(e.message())
}

/// uint32_t conversion with the module's error conventions.
fn u32_arg(o: &PyObject) -> PyResult<u32> {
    if !int::is_int(o) && !ab::has_index(o) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(o));
    }
    let i = ab::index(o)?;
    if int::sign(&i) < 0 {
        raise!(ValueError, "Value must be non-negative");
    }
    match int::as_i64(&i) {
        Some(v) if v <= u32::MAX as i64 => Ok(v as u32),
        _ => raise!(OverflowError, "Value too large for uint32_t type"),
    }
}

fn u64_arg(o: &PyObject) -> PyResult<u64> {
    if !int::is_int(o) && !ab::has_index(o) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(o));
    }
    let i = ab::index(o)?;
    if int::sign(&i) < 0 {
        raise!(ValueError, "Value must be non-negative");
    }
    cc::c_u64(&i)
}

fn format_arg(o: Option<&PyObject>, default: i32) -> PyResult<i32> {
    match o {
        None => Ok(default),
        Some(o) => cc::c_int(o),
    }
}

fn preset_options(p: u32) -> PyResult<LzOptions> {
    match LzOptions::preset(p) {
        Some(o) => Ok(o),
        None => Err(lzma_error(&format!("Invalid compression preset: {}", p))),
    }
}

/// The keys of a filter-specifier mapping (as Rust strings), with their values.
fn spec_items(spec: &PyObject) -> PyResult<Vec<(String, PyRef)>> {
    let keys = if dict::is_dict(spec) { ab::call_method_str(spec, "keys", &[])? } else { ab::call_method_str(spec, "keys", &[])? };
    let mut out = Vec::new();
    for k in ab::collect_iterable(&keys)? {
        let v = ab::getitem(spec, &k)?;
        let name = if pystr::is_str(&k) { pystr::to_string_lossy(&k) } else { String::new() };
        out.push((name, v));
    }
    Ok(out)
}

/// Convert a filter specifier mapping into a `FilterSpec`.
fn parse_filter(spec: &PyObject) -> PyResult<FilterSpec> {
    if !dict::is_dict(spec) && ab::lookup_special(spec, name!("__getitem__"))?.is_none() {
        raise!(TypeError, "Filter specifier must be a dict or dict-like object");
    }
    let idv = match ab::getitem(spec, &pystr::new_str("id")) {
        Ok(v) => v,
        Err(e) => {
            if exc_matches(crate::types::exceptions::KeyError()) {
                clear_exc();
                raise!(ValueError, "Filter specifier must have an \"id\" entry");
            }
            return Err(e);
        }
    };
    let id = u64_arg(&idv)?;
    let items = spec_items(spec)?;
    if id == FILTER_LZMA1 || id == FILTER_LZMA2 {
        let mut preset = 6u32;
        for (k, v) in &items {
            if k == "preset" {
                preset = u32_arg(v)?;
            }
        }
        let mut o = preset_options(preset)?;
        for (k, v) in &items {
            match k.as_str() {
                "id" | "preset" => {}
                "dict_size" => o.dict_size = u32_arg(v)?,
                "lc" => o.props.lc = u32_arg(v)?,
                "lp" => o.props.lp = u32_arg(v)?,
                "pb" => o.props.pb = u32_arg(v)?,
                "mode" => o.mode = u32_arg(v)?,
                "nice_len" => o.nice_len = u32_arg(v)?,
                "mf" => o.mf = u32_arg(v)?,
                "depth" => o.depth = u32_arg(v)?,
                _ => raise!(ValueError, "Invalid filter specifier for LZMA filter"),
            }
        }
        return Ok(FilterSpec::lzma(id, o));
    }
    if id == FILTER_DELTA {
        let mut f = FilterSpec::simple(id);
        for (k, v) in &items {
            match k.as_str() {
                "id" => {}
                "dist" => f.dist = u32_arg(v)?,
                _ => raise!(ValueError, "Invalid filter specifier for delta filter"),
            }
        }
        return Ok(f);
    }
    if is_bcj(id) && id != FILTER_RISCV {
        let mut f = FilterSpec::simple(id);
        for (k, v) in &items {
            match k.as_str() {
                "id" => {}
                "start_offset" => f.start_offset = u32_arg(v)?,
                _ => raise!(ValueError, "Invalid filter specifier for BCJ filter"),
            }
        }
        return Ok(f);
    }
    raise!(ValueError, "Invalid filter ID: {}", id);
}

fn parse_chain(o: &PyObject) -> PyResult<Vec<FilterSpec>> {
    if !crate::types::list::is_list(o) && !crate::types::tuple::is_tuple(o) && ab::lookup_special(o, name!("__len__"))?.is_none() {
        raise!(TypeError, "filters must be a sequence of filter specifiers");
    }
    if pystr::is_str(o) || crate::types::bytes::is_bytes(o) || dict::is_dict(o) {
        raise!(TypeError, "filters must be a sequence of filter specifiers");
    }
    let n = ab::length(o)?;
    if n > 4 {
        raise!(ValueError, "Too many filters - liblzma supports a maximum of 4");
    }
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let item = ab::getitem(o, &int::new_int(i as i64))?;
        v.push(parse_filter(&item)?);
    }
    Ok(v)
}

// ------------------------------------------------------------------ compressor

struct Comp {
    enc: Encoder,
    flushed: bool,
}

unsafe extern "C" fn comp_new(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (v, kn) = args::kwdict_to_vectorcall(&*a, kw);
            let vr: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            static P: Params = Params::new("LZMACompressor", &["format", "check", "preset", "filters"], 0, 0);
            let [format, check, preset, filters] = args::parse::<4>(&P, &vr, kn.as_deref())?;
            let format = format_arg(format, FORMAT_XZ)?;
            let check = match check {
                Some(c) => cc::c_int(c)?,
                None => -1,
            };
            let preset = match preset {
                Some(p) if !cc::is_none(p) => Some(u32_arg(p)?),
                _ => None,
            };
            let filters = filters.filter(|f| !cc::is_none(f));
            if format != FORMAT_XZ && check != -1 && check != 0 {
                raise!(ValueError, "Integrity checks are only supported by FORMAT_XZ");
            }
            if preset.is_some() && filters.is_some() {
                raise!(ValueError, "Cannot specify both preset and filter chain");
            }
            let fmt = match format {
                FORMAT_XZ => Format::Xz,
                FORMAT_ALONE => Format::Alone,
                FORMAT_RAW => Format::Raw,
                _ => raise!(ValueError, "Invalid container format: {}", format),
            };
            let chain = match filters {
                Some(f) => parse_chain(f)?,
                None => {
                    if fmt == Format::Raw {
                        raise!(ValueError, "Must specify filters for FORMAT_RAW");
                    }
                    let o = preset_options(preset.unwrap_or(6))?;
                    let id = if fmt == Format::Alone { FILTER_LZMA1 } else { FILTER_LZMA2 };
                    vec![FilterSpec::lzma(id, o)]
                }
            };
            let check = if check == -1 { if fmt == Format::Xz { lz::CHECK_CRC64 } else { 0 } } else { check as u32 };
            let enc = Encoder::new(fmt, check, chain).map_err(codec_err)?;
            cc::new_obj(t, Comp { enc, flushed: false })
        })();
        into_c(r)
    }
}

fn c_compress(o: &PyObject, data: &PyObject) -> PyResult {
    let b = cc::buffer(data)?;
    let mut st = cc::lock::<Comp>(o)?;
    if st.flushed {
        raise!(ValueError, "Compressor has been flushed");
    }
    let d = b.data();
    let out = cc::maybe_blocking(d.len(), || {
        let mut out = Vec::new();
        st.enc.compress(d, &mut out);
        out
    });
    cc::new_bytes(&out)
}

fn c_flush(o: &PyObject) -> PyResult {
    let mut st = cc::lock::<Comp>(o)?;
    if st.flushed {
        raise!(ValueError, "Repeated call to flush()");
    }
    st.flushed = true;
    let out = crate::threads::blocking(|| {
        let mut out = Vec::new();
        st.enc.finish(&mut out);
        out
    });
    cc::new_bytes(&out)
}

// ------------------------------------------------------------------ decompressor

struct Decomp {
    dec: Decoder,
    eof: bool,
    needs_input: bool,
    unused: Vec<u8>,
    failed: Option<lz::Error>,
}

fn make_decoder(format: Option<&PyObject>, memlimit: Option<&PyObject>, filters: Option<&PyObject>) -> PyResult<Decomp> {
    let format = format_arg(format, FORMAT_AUTO)?;
    let memlimit = match memlimit {
        Some(m) if !cc::is_none(m) => Some(u64_arg(m)?),
        _ => None,
    };
    let filters = filters.filter(|f| !cc::is_none(f));
    if format == FORMAT_RAW && memlimit.is_some() {
        raise!(ValueError, "Cannot specify memory limit with FORMAT_RAW");
    }
    if format == FORMAT_RAW && filters.is_none() {
        raise!(ValueError, "Must specify filters for FORMAT_RAW");
    }
    if format != FORMAT_RAW && filters.is_some() {
        raise!(ValueError, "Cannot specify filters except with FORMAT_RAW");
    }
    let fmt = match format {
        FORMAT_AUTO => Format::Auto,
        FORMAT_XZ => Format::Xz,
        FORMAT_ALONE => Format::Alone,
        FORMAT_RAW => Format::Raw,
        _ => raise!(ValueError, "Invalid container format: {}", format),
    };
    let chain = match filters {
        Some(f) => Some(parse_chain(f)?),
        None => None,
    };
    let dec = Decoder::new(fmt, memlimit, chain).map_err(codec_err)?;
    Ok(Decomp { dec, eof: false, needs_input: true, unused: Vec::new(), failed: None })
}

unsafe extern "C" fn decomp_new(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (v, kn) = args::kwdict_to_vectorcall(&*a, kw);
            let vr: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            static P: Params = Params::new("LZMADecompressor", &["format", "memlimit", "filters"], 0, 0);
            let [format, memlimit, filters] = args::parse::<3>(&P, &vr, kn.as_deref())?;
            let st = make_decoder(format, memlimit, filters)?;
            cc::new_obj(t, st)
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
        raise!(EOFError, "Already at end of stream");
    }
    if let Some(e) = st.failed {
        return Err(codec_err(e));
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
            st.failed = Some(e);
            st.needs_input = false;
            return Err(codec_err(e));
        }
    };
    if st.dec.is_done() {
        st.eof = true;
        st.needs_input = false;
        st.unused = st.dec.unused().to_vec();
    } else {
        st.needs_input = !(limit != usize::MAX && out.len() == limit);
    }
    cc::new_bytes(&out)
}

fn d_eof(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(cc::lock::<Decomp>(o)?.eof))
}
fn d_unused(o: &PyObject) -> PyResult {
    cc::new_bytes(&cc::lock::<Decomp>(o)?.unused)
}
fn d_needs_input(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(cc::lock::<Decomp>(o)?.needs_input))
}
fn d_check(o: &PyObject) -> PyResult {
    Ok(int::new_int(cc::lock::<Decomp>(o)?.dec.check() as i64))
}

// ------------------------------------------------------------------ module functions

fn m_is_check_supported(_m: &PyObject, a: &PyObject) -> PyResult {
    let id = cc::c_int(a)?;
    Ok(bool_::new_bool(id >= 0 && lz::check_supported(id as u32)))
}

fn m_encode_filter_properties(_m: &PyObject, a: &PyObject) -> PyResult {
    let f = parse_filter(a)?;
    let p = f.encode_props().map_err(codec_err)?;
    cc::new_bytes(&p)
}

fn m_decode_filter_properties(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("_decode_filter_properties", a, 2, 2)?;
    let id = u64_arg(a[0])?;
    let b = cc::buffer(a[1])?;
    let f = FilterSpec::decode_props(id, b.data()).map_err(codec_err)?;
    let d = dict::new_dict();
    dict::set_item_str(&d, "id", &int::new_int_u128(id as u128))?;
    if let Some(o) = f.lz {
        if id == FILTER_LZMA1 {
            cc::set_int(&d, "lc", o.props.lc as i64)?;
            cc::set_int(&d, "lp", o.props.lp as i64)?;
            cc::set_int(&d, "pb", o.props.pb as i64)?;
        }
        cc::set_int(&d, "dict_size", o.dict_size as i64)?;
    } else if id == FILTER_DELTA {
        cc::set_int(&d, "dist", f.dist as i64)?;
    } else if f.start_offset != 0 {
        cc::set_int(&d, "start_offset", f.start_offset as i64)?;
    }
    Ok(d)
}

static INIT: std::sync::Once = std::sync::Once::new();

pub fn init_module() -> PyResult {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        cc::init_state_type::<Comp>(ctype(), c"_lzma.LZMACompressor", Py_TPFLAGS_BASETYPE as u64);
        let t = &mut *ctype();
        t.tp_new = Some(comp_new);
        t.tp_doc = c"LZMACompressor(format=FORMAT_XZ, check=-1, preset=None, filters=None)\n\nIncremental .xz / .lzma / raw compressor.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("compress", O, c_compress, "compress($self, data, /)\n--\n\nFeed data; returns whatever compressed output is ready."),
            ("flush", NOARGS, c_flush, "flush($self, /)\n--\n\nEnd the stream and return the remaining output."),
            ("__reduce__", NOARGS, cc::no_pickle),
        ];
        crate::methods::lock_self_methods_held(ctype());
        res = type_ready(ctype());
        if res.is_err() {
            return;
        }
        cc::init_state_type::<Decomp>(dtype(), c"_lzma.LZMADecompressor", Py_TPFLAGS_BASETYPE as u64);
        let t = &mut *dtype();
        t.tp_new = Some(decomp_new);
        t.tp_doc = c"LZMADecompressor(format=FORMAT_AUTO, memlimit=None, filters=None)\n--\n\nIncremental decompressor for one .xz / .lzma / raw stream.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("decompress", FASTKW, d_decompress, "decompress($self, /, data, max_length=-1)\n--\n\nDecode data (at most max_length output bytes when max_length >= 0)."),
            ("__reduce__", NOARGS, cc::no_pickle),
        ];
        t.tp_getset = crate::getset_table![
            ("check", d_check; "Integrity check id of the stream (CHECK_UNKNOWN until known)."),
            ("eof", d_eof; "Whether the end of the stream was reached."),
            ("unused_data", d_unused; "Bytes that followed the end of the stream."),
            ("needs_input", d_needs_input; "Whether decompress() must be given more input to make progress."),
        ];
        crate::methods::lock_self_methods_held(dtype());
        res = type_ready(dtype());
    });
    res?;
    let m = crate::types::module::new_module("_lzma")?;
    let d = crate::types::module::module_dict(&m);
    let err = ERROR.get_or_create("_lzma", "LZMAError", crate::types::exceptions::Exception())?;
    dict::set_item_str(&d, "LZMAError", &type_ref(err))?;
    dict::set_item_str(&d, "LZMACompressor", &type_ref(ctype()))?;
    dict::set_item_str(&d, "LZMADecompressor", &type_ref(dtype()))?;
    for (k, v) in [
        ("FORMAT_AUTO", FORMAT_AUTO as i64),
        ("FORMAT_XZ", FORMAT_XZ as i64),
        ("FORMAT_ALONE", FORMAT_ALONE as i64),
        ("FORMAT_RAW", FORMAT_RAW as i64),
        ("CHECK_NONE", 0),
        ("CHECK_CRC32", 1),
        ("CHECK_CRC64", 4),
        ("CHECK_SHA256", 10),
        ("CHECK_ID_MAX", 15),
        ("CHECK_UNKNOWN", 16),
        ("FILTER_DELTA", FILTER_DELTA as i64),
        ("FILTER_X86", FILTER_X86 as i64),
        ("FILTER_POWERPC", FILTER_POWERPC as i64),
        ("FILTER_IA64", FILTER_IA64 as i64),
        ("FILTER_ARM", FILTER_ARM as i64),
        ("FILTER_ARMTHUMB", FILTER_ARMTHUMB as i64),
        ("FILTER_SPARC", FILTER_SPARC as i64),
        ("FILTER_LZMA2", FILTER_LZMA2 as i64),
        ("FILTER_LZMA1", FILTER_LZMA1 as i64),
        ("MF_HC3", 0x03),
        ("MF_HC4", 0x04),
        ("MF_BT2", 0x12),
        ("MF_BT3", 0x13),
        ("MF_BT4", 0x14),
        ("MODE_FAST", 1),
        ("MODE_NORMAL", 2),
        ("PRESET_DEFAULT", 6),
        ("PRESET_EXTREME", lz::PRESET_EXTREME as i64),
    ] {
        cc::set_int(&d, k, v)?;
    }
    let methods = crate::mdefs![
        ("is_check_supported", O, m_is_check_supported, "is_check_supported($module, check_id, /)\n--\n\nTrue if this build can compute and verify the given check type."),
        ("_encode_filter_properties", O, m_encode_filter_properties, "_encode_filter_properties($module, filter, /)\n--\n\nSerialize the options of a filter specifier dict to raw property bytes."),
        ("_decode_filter_properties", FAST, m_decode_filter_properties, "_decode_filter_properties($module, filter_id, encoded_props, /)\n--\n\nBuild a filter specifier dict from a filter id and raw property bytes."),
    ];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    Ok(m)
}
