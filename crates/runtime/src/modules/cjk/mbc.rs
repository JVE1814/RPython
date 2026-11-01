//! `_multibytecodec`: the codec object returned by `_codecs_xx.getcodec()`,
//! the incremental encoder/decoder and stream reader/writer base classes, and
//! the conversion driver that applies the error handlers.

use super::{CodecDef, Fail, St};
use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::modules::compress_common as cc;
use crate::object::*;
use crate::raise;
use crate::types::exceptions::{self as exc, UnicodeErrorExc};
use crate::types::str as pystr;
use crate::types::typeobject::*;
use crate::types::{bool_, bytes, dict, int, none, tuple};

#[unsafe(no_mangle)]
pub static mut _PyMultibyteCodec_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyMultibyteIncrementalEncoder_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyMultibyteIncrementalDecoder_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyMultibyteStreamReader_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyMultibyteStreamWriter_Type: PyTypeObject = PyTypeObject::ZERO;

fn codec_type() -> *mut PyTypeObject {
    &raw mut _PyMultibyteCodec_Type
}
fn ienc_type() -> *mut PyTypeObject {
    &raw mut _PyMultibyteIncrementalEncoder_Type
}
fn idec_type() -> *mut PyTypeObject {
    &raw mut _PyMultibyteIncrementalDecoder_Type
}
fn reader_type() -> *mut PyTypeObject {
    &raw mut _PyMultibyteStreamReader_Type
}
fn writer_type() -> *mut PyTypeObject {
    &raw mut _PyMultibyteStreamWriter_Type
}

const MAX_ENC_PENDING: usize = 8; // UTF-8 bytes of unencoded input kept by getstate()
const MAX_DEC_PENDING: usize = 8;

// ===================================================================== driver

/// The error handling scheme of one conversion.
enum Errors<'a> {
    Strict,
    Ignore,
    Replace,
    Custom(&'a str),
}

fn errors_of(s: &str) -> Errors<'_> {
    match s {
        "strict" => Errors::Strict,
        "ignore" => Errors::Ignore,
        "replace" => Errors::Replace,
        other => Errors::Custom(other),
    }
}

fn set_exc_range(e: &PyObject, start: usize, end: usize, reason: &str) {
    let u = unsafe { &mut *(e.as_ptr() as *mut UnicodeErrorExc) };
    u.start = start as isize;
    u.end = end as isize;
    let r = pystr::new_str(reason).into_raw();
    let old = std::mem::replace(&mut u.reason, r);
    unsafe { xdecref(old) };
}

/// Lazily created exception object, reused for every error of one call.
struct ExcCache {
    exc: Option<PyRef>,
}

impl ExcCache {
    fn get(&mut self, tp: *mut PyTypeObject, encoding: &str, obj: &dyn Fn() -> PyRef, start: usize, end: usize, reason: &str) -> PyResult {
        if let Some(e) = &self.exc {
            set_exc_range(e, start, end, reason);
            return Ok(e.clone());
        }
        let e = exc::make_exception(
            tp,
            &[pystr::new_str(encoding), obj(), int::new_int(start as i64), int::new_int(end as i64), pystr::new_str(reason)],
        )?;
        self.exc = Some(e.clone());
        Ok(e)
    }
}

fn fail_info(f: Fail, left: usize) -> PyResult<(&'static str, usize)> {
    match f {
        Fail::Illegal(n) => Ok(("illegal multibyte sequence", n.min(left).max(1))),
        Fail::TooFew => Ok(("incomplete multibyte sequence", left)),
        Fail::Internal => raise!(RuntimeError, "internal codec error"),
    }
}

/// The position returned by an error handler, validated against `len`.
fn handler_pos(p: &PyObject, len: usize) -> PyResult<usize> {
    let v = match int::as_i64(p) {
        Some(v) => v,
        None => raise!(IndexError, "position {} from error handler out of bounds", ab::repr(p).map(|r| pystr::to_string_lossy(&r)).unwrap_or_default()),
    };
    let v = if v < 0 { v + len as i64 } else { v };
    if v < 0 || v > len as i64 {
        raise!(IndexError, "position {} from error handler out of bounds", v);
    }
    Ok(v as usize)
}

/// Encode `inp` with `def` starting from state `st`.  Returns the bytes and
/// the number of characters consumed (fewer than `inp.len()` only when the
/// input ends with a character that may combine with the next one and
/// `flush` is false).
fn encode_core(def: &CodecDef, st: &mut St, inp: &[u32], errors: &str, flush: bool) -> PyResult<(Vec<u8>, usize)> {
    let mut out = Vec::with_capacity(inp.len() * 2);
    let mut pos = 0;
    let mut cache = ExcCache { exc: None };
    let mut handler: Option<PyRef> = None;
    let obj = std::cell::OnceCell::new();
    loop {
        let f = match def.imp.encode(st, inp, &mut pos, &mut out, flush) {
            Ok(()) => break,
            Err(Fail::TooFew) if !flush => break,
            Err(f) => f,
        };
        let (reason, esize) = fail_info(f, inp.len() - pos)?;
        let end = pos + esize;
        let make_obj = || -> PyRef { obj.get_or_init(|| pystr::from_codepoints(inp)).clone() };
        match errors_of(errors) {
            Errors::Strict => {
                let e = cache.get(exc::UnicodeEncodeError(), def.name, &make_obj, pos, end, reason)?;
                set_exc(e);
                return Err(PyErr);
            }
            Errors::Ignore => pos = end,
            Errors::Replace => {
                let mut p = 0;
                let mut tmp = Vec::new();
                if def.imp.encode(st, &['?' as u32], &mut p, &mut tmp, false).is_ok() && p == 1 {
                    out.extend_from_slice(&tmp);
                } else {
                    out.push(b'?');
                }
                pos = end;
            }
            Errors::Custom(name) => {
                if handler.is_none() {
                    handler = Some(crate::modules::_codecs::lookup_error(name)?);
                }
                let e = cache.get(exc::UnicodeEncodeError(), def.name, &make_obj, pos, end, reason)?;
                let r = ab::call(handler.as_ref().unwrap(), &[&e])?;
                let bad = || err!(TypeError, "encoding error handler must return (str, int) tuple");
                if !tuple::is_tuple(&r) || tuple::tuple_len(&r) != 2 {
                    return Err(bad());
                }
                let rep = tuple::tuple_get(&r, 0);
                let np = tuple::tuple_get(&r, 1);
                if !int::is_int(np) {
                    return Err(bad());
                }
                if pystr::is_str(rep) {
                    let chars = pystr::str_data(rep).to_vec();
                    let (b, _) = encode_core(def, st, &chars, "strict", true)?;
                    out.extend_from_slice(&b);
                } else if bytes::is_bytes(rep) {
                    out.extend_from_slice(bytes::bytes_data(rep));
                } else {
                    return Err(bad());
                }
                pos = handler_pos(np, inp.len())?;
            }
        }
    }
    Ok((out, pos))
}

/// Decode `data` with `def` from state `st`.  With `final_`, an incomplete
/// sequence at the end is an error; otherwise decoding stops before it.
fn decode_core(def: &CodecDef, st: &mut St, data: &[u8], errors: &str, final_: bool) -> PyResult<(Vec<u32>, usize)> {
    let mut out = Vec::with_capacity(data.len());
    let mut pos = 0;
    let mut cache = ExcCache { exc: None };
    let mut handler: Option<PyRef> = None;
    let obj = std::cell::OnceCell::new();
    loop {
        let f = match def.imp.decode(st, data, &mut pos, &mut out) {
            Ok(()) => break,
            Err(Fail::TooFew) if !final_ => break,
            Err(f) => f,
        };
        let (reason, esize) = fail_info(f, data.len() - pos)?;
        let end = pos + esize;
        let make_obj = || -> PyRef { obj.get_or_init(|| bytes::new_bytes(data)).clone() };
        match errors_of(errors) {
            Errors::Strict => {
                let e = cache.get(exc::UnicodeDecodeError(), def.name, &make_obj, pos, end, reason)?;
                set_exc(e);
                return Err(PyErr);
            }
            Errors::Ignore => pos = end,
            Errors::Replace => {
                out.push(0xFFFD);
                pos = end;
            }
            Errors::Custom(name) => {
                if handler.is_none() {
                    handler = Some(crate::modules::_codecs::lookup_error(name)?);
                }
                let e = cache.get(exc::UnicodeDecodeError(), def.name, &make_obj, pos, end, reason)?;
                let r = ab::call(handler.as_ref().unwrap(), &[&e])?;
                let bad = || err!(TypeError, "decoding error handler must return (str, int) tuple");
                if !tuple::is_tuple(&r) || tuple::tuple_len(&r) != 2 {
                    return Err(bad());
                }
                let rep = tuple::tuple_get(&r, 0);
                let np = tuple::tuple_get(&r, 1);
                if !pystr::is_str(rep) || !int::is_int(np) {
                    return Err(bad());
                }
                out.extend(pystr::str_data(rep).iter());
                pos = handler_pos(np, data.len())?;
            }
        }
    }
    Ok((out, pos))
}

// ============================================================ argument helpers

fn errors_arg(o: Option<&PyObject>) -> PyResult<String> {
    match o {
        None => Ok("strict".to_string()),
        Some(o) if none::is_none(o) => Ok("strict".to_string()),
        Some(o) if pystr::is_str(o) => Ok(pystr::to_string_lossy(o)),
        Some(o) => raise!(TypeError, "errors must be a string, not {}", tname(o)),
    }
}

/// `str` input of the encoders (other objects are converted with str()).
fn text_arg(o: &PyObject) -> PyResult<PyRef> {
    if pystr::is_str(o) {
        return Ok(o.to_ref());
    }
    let s = ab::str(o)?;
    if !pystr::is_str(&s) {
        raise!(TypeError, "couldn't convert the object to unicode.");
    }
    Ok(s)
}

// =============================================================== MultibyteCodec

fn codec_of(o: &PyObject) -> PyResult<&'static CodecDef> {
    Ok(*cc::lock::<&'static CodecDef>(o)?)
}

pub fn new_codec(def: &'static CodecDef) -> PyResult {
    init_types()?;
    cc::new_obj(codec_type(), def)
}

fn codec_encode(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("encode", &["input", "errors"], 0, 1);
    let [input, errors] = args::parse::<2>(&P, a, kw)?;
    let def = codec_of(o)?;
    let errors = errors_arg(errors)?;
    let s = text_arg(input.unwrap())?;
    let chars = pystr::str_data(&s).to_vec();
    let mut st = [0; 8];
    def.imp.enc_init(&mut st);
    let (mut out, _) = encode_core(def, &mut st, &chars, &errors, true)?;
    def.imp.enc_reset(&mut st, &mut out);
    Ok(tuple::new_tuple(vec![bytes::new_bytes(&out), int::new_int(chars.len() as i64)]))
}

fn codec_decode(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("decode", &["input", "errors"], 0, 1);
    let [input, errors] = args::parse::<2>(&P, a, kw)?;
    let def = codec_of(o)?;
    let errors = errors_arg(errors)?;
    let buf = cc::buffer(input.unwrap())?;
    let data = buf.data();
    let mut st = [0; 8];
    def.imp.dec_init(&mut st);
    let (chars, _) = decode_core(def, &mut st, data, &errors, true)?;
    Ok(tuple::new_tuple(vec![pystr::from_codepoints(&chars), int::new_int(data.len() as i64)]))
}

// ====================================== incremental encoder/decoder, streams

struct Coder {
    codec: &'static CodecDef,
    errors: String,
    st: St,
    pending_text: Vec<u32>,
    pending_bytes: Vec<u8>,
    stream: Option<PyRef>,
}

fn coder(o: &PyObject) -> PyResult<std::sync::MutexGuard<'static, Coder>> {
    cc::lock::<Coder>(o)
}

/// Snapshot of a coder's mutable part, taken so that no lock is held while
/// error handlers or stream methods run.
struct Snap {
    codec: &'static CodecDef,
    errors: String,
    st: St,
    pending_text: Vec<u32>,
    pending_bytes: Vec<u8>,
    stream: Option<PyRef>,
}

fn snap(o: &PyObject) -> PyResult<Snap> {
    let c = coder(o)?;
    Ok(Snap {
        codec: c.codec,
        errors: c.errors.clone(),
        st: c.st,
        pending_text: c.pending_text.clone(),
        pending_bytes: c.pending_bytes.clone(),
        stream: c.stream.clone(),
    })
}

fn new_coder(tp: *mut PyTypeObject, errors: Option<&PyObject>, stream: Option<&PyObject>, decoder: bool) -> PyResult {
    let codec = ab::getattr_str(type_obj(tp), "codec")?;
    if codec.type_ptr() != codec_type() {
        raise!(TypeError, "codec is unexpected type");
    }
    let def = codec_of(&codec)?;
    let errors = errors_arg(errors)?;
    let mut st = [0; 8];
    if decoder {
        def.imp.dec_init(&mut st);
    } else {
        def.imp.enc_init(&mut st);
    }
    cc::new_obj(tp, Coder { codec: def, errors, st, pending_text: Vec::new(), pending_bytes: Vec::new(), stream: stream.map(|s| s.to_ref()) })
}

unsafe fn tp_new_common(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject, stream: bool, decoder: bool) -> *mut PyObject {
    let r = (|| -> PyResult {
        let (v, kn) = args::kwdict_to_vectorcall(unsafe { &*a }, kw);
        let vr: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
        if stream {
            static P: Params = Params::new("MultibyteStream", &["stream", "errors"], 0, 1);
            let [s, e] = args::parse::<2>(&P, &vr, kn.as_deref())?;
            new_coder(t, e, s, decoder)
        } else {
            static P: Params = Params::new("MultibyteIncremental", &["errors"], 0, 0);
            let [e] = args::parse::<1>(&P, &vr, kn.as_deref())?;
            new_coder(t, e, None, decoder)
        }
    })();
    into_c(r)
}

unsafe extern "C" fn ienc_new(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { tp_new_common(t, a, kw, false, false) }
}
unsafe extern "C" fn idec_new(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { tp_new_common(t, a, kw, false, true) }
}
unsafe extern "C" fn reader_new(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { tp_new_common(t, a, kw, true, true) }
}
unsafe extern "C" fn writer_new(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { tp_new_common(t, a, kw, true, false) }
}

/// `__init__` accepts and ignores the constructor arguments.
unsafe extern "C" fn noop_init(_o: *mut PyObject, _a: *mut PyObject, _kw: *mut PyObject) -> std::ffi::c_int {
    0
}

fn get_errors(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(&coder(o)?.errors))
}

fn set_errors(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let Some(v) = v else { raise!(AttributeError, "cannot delete attribute") };
    if !pystr::is_str(v) {
        raise!(TypeError, "errors must be a string");
    }
    coder(o)?.errors = pystr::to_string_lossy(v);
    Ok(())
}

fn get_stream(o: &PyObject) -> PyResult {
    Ok(match &coder(o)?.stream {
        Some(s) => s.clone(),
        None => none::none_ref(),
    })
}

/// Stateful encode of `text` (after the pending characters).  On error the
/// coder is left unchanged.
fn encode_stateful(o: &PyObject, text: &PyObject, final_: bool) -> PyResult<Vec<u8>> {
    let s = text_arg(text)?;
    let sn = snap(o)?;
    let mut data = sn.pending_text;
    data.extend(pystr::str_data(&s).iter());
    let mut st = sn.st;
    let (mut out, used) = encode_core(sn.codec, &mut st, &data, &sn.errors, final_)?;
    let rest = if final_ {
        sn.codec.imp.enc_reset(&mut st, &mut out);
        Vec::new()
    } else {
        data[used..].to_vec()
    };
    if rest.len() > MAX_ENC_PENDING {
        raise!(UnicodeError, "pending buffer overflow");
    }
    let mut c = coder(o)?;
    c.st = st;
    c.pending_text = rest;
    Ok(out)
}

fn ienc_encode(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("encode", &["input", "final"], 0, 1);
    let [input, fin] = args::parse::<2>(&P, a, kw)?;
    let final_ = match fin {
        Some(f) => ab::is_true(f)?,
        None => false,
    };
    Ok(bytes::new_bytes(&encode_stateful(o, input.unwrap(), final_)?))
}

fn utf8_of(chars: &[u32]) -> Vec<u8> {
    let mut v = Vec::new();
    for &c in chars {
        match c {
            0..=0x7F => v.push(c as u8),
            0x80..=0x7FF => v.extend_from_slice(&[0xC0 | (c >> 6) as u8, 0x80 | (c & 0x3F) as u8]),
            0x800..=0xFFFF => v.extend_from_slice(&[0xE0 | (c >> 12) as u8, 0x80 | (c >> 6 & 0x3F) as u8, 0x80 | (c & 0x3F) as u8]),
            _ => v.extend_from_slice(&[0xF0 | (c >> 18) as u8, 0x80 | (c >> 12 & 0x3F) as u8, 0x80 | (c >> 6 & 0x3F) as u8, 0x80 | (c & 0x3F) as u8]),
        }
    }
    v
}

/// Non-negative int from little-endian bytes.
fn int_from_le(b: &[u8]) -> PyRef {
    let mut digits = Vec::new();
    let mut acc: u64 = 0;
    let mut nbits = 0;
    for &x in b {
        acc |= (x as u64) << nbits;
        nbits += 8;
        while nbits >= 30 {
            digits.push((acc & 0x3FFF_FFFF) as u32);
            acc >>= 30;
            nbits -= 30;
        }
    }
    if nbits > 0 {
        digits.push(acc as u32);
    }
    while digits.last() == Some(&0) {
        digits.pop();
    }
    int::from_digits(false, &digits)
}

/// `n` little-endian bytes of a non-negative int (OverflowError otherwise).
fn int_to_le(o: &PyObject, n: usize) -> PyResult<Vec<u8>> {
    if !int::is_int(o) {
        raise!(TypeError, "an integer is required (got type {})", tname(o));
    }
    if int::sign(o) < 0 {
        raise!(OverflowError, "can't convert negative int to unsigned");
    }
    let mut out = Vec::with_capacity(n);
    let mut acc: u64 = 0;
    let mut nbits = 0;
    for &d in int::digits(o) {
        acc |= (d as u64) << nbits;
        nbits += 30;
        while nbits >= 8 {
            out.push(acc as u8);
            acc >>= 8;
            nbits -= 8;
        }
    }
    if nbits > 0 {
        out.push(acc as u8);
    }
    while out.len() > n {
        if out.pop() != Some(0) {
            raise!(OverflowError, "int too big to convert");
        }
    }
    out.resize(n, 0);
    Ok(out)
}

fn ienc_getstate(o: &PyObject) -> PyResult {
    let c = coder(o)?;
    let pend = utf8_of(&c.pending_text);
    if pend.len() > MAX_ENC_PENDING {
        raise!(UnicodeError, "pending buffer too large");
    }
    let mut b = vec![pend.len() as u8];
    b.extend_from_slice(&pend);
    b.extend_from_slice(&c.st);
    Ok(int_from_le(&b))
}

fn ienc_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    let b = int_to_le(state, 1 + MAX_ENC_PENDING + 8)?;
    let n = b[0] as usize;
    if n > MAX_ENC_PENDING {
        raise!(UnicodeError, "pending buffer too large");
    }
    let text = crate::modules::codecs_core::decode_utf8(&b[1..1 + n], "strict")?;
    let mut st = [0; 8];
    st.copy_from_slice(&b[1 + n..1 + n + 8]);
    let mut c = coder(o)?;
    c.pending_text = pystr::str_data(&text).to_vec();
    c.st = st;
    Ok(none::none_ref())
}

fn ienc_reset(o: &PyObject) -> PyResult {
    let mut c = coder(o)?;
    let mut sink = Vec::new();
    let mut st = c.st;
    c.codec.imp.enc_reset(&mut st, &mut sink);
    c.st = st;
    c.pending_text.clear();
    Ok(none::none_ref())
}

/// Stateful decode of `data` (after the pending bytes).
fn decode_stateful(o: &PyObject, data: &[u8], final_: bool) -> PyResult<PyRef> {
    let sn = snap(o)?;
    let mut buf = sn.pending_bytes;
    buf.extend_from_slice(data);
    let mut st = sn.st;
    let (chars, used) = decode_core(sn.codec, &mut st, &buf, &sn.errors, final_)?;
    let rest = buf[used..].to_vec();
    if rest.len() > MAX_DEC_PENDING {
        raise!(UnicodeError, "pending buffer overflow");
    }
    let mut c = coder(o)?;
    c.st = st;
    c.pending_bytes = rest;
    Ok(pystr::from_codepoints(&chars))
}

fn idec_decode(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("decode", &["input", "final"], 0, 1);
    let [input, fin] = args::parse::<2>(&P, a, kw)?;
    let final_ = match fin {
        Some(f) => ab::is_true(f)?,
        None => false,
    };
    let buf = cc::buffer(input.unwrap())?;
    decode_stateful(o, buf.data(), final_)
}

fn idec_getstate(o: &PyObject) -> PyResult {
    let c = coder(o)?;
    Ok(tuple::new_tuple(vec![bytes::new_bytes(&c.pending_bytes), int_from_le(&c.st)]))
}

fn idec_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    let bad = || err!(TypeError, "setstate(): illegal state argument");
    if !tuple::is_tuple(state) || tuple::tuple_len(state) != 2 {
        return Err(bad());
    }
    let pend = tuple::tuple_get(state, 0);
    let flags = tuple::tuple_get(state, 1);
    if !bytes::is_bytes(pend) || !int::is_int(flags) {
        return Err(bad());
    }
    let data = bytes::bytes_data(pend);
    let name = coder(o)?.codec.name;
    if data.len() > MAX_DEC_PENDING {
        let e = exc::make_exception(
            exc::UnicodeDecodeError(),
            &[pystr::new_str(name), pend.to_ref(), int::new_int(0), int::new_int(data.len() as i64), pystr::new_str("pending buffer too large")],
        )?;
        set_exc(e);
        return Err(PyErr);
    }
    let b = int_to_le(flags, 8)?;
    let mut c = coder(o)?;
    c.pending_bytes = data.to_vec();
    c.st.copy_from_slice(&b);
    Ok(none::none_ref())
}

fn idec_reset(o: &PyObject) -> PyResult {
    let mut c = coder(o)?;
    let mut st = [0; 8];
    c.codec.imp.dec_init(&mut st);
    c.st = st;
    c.pending_bytes.clear();
    Ok(none::none_ref())
}

// --------------------------------------------------------------- stream reader

fn size_arg(o: Option<&PyObject>) -> PyResult<isize> {
    match o {
        None => Ok(-1),
        Some(o) if none::is_none(o) => Ok(-1),
        Some(o) if int::is_int(o) => {
            let v = ab::index_as_ssize(o, None)?;
            Ok(if v < 0 { -1 } else { v })
        }
        Some(_) => raise!(TypeError, "arg 1 must be an integer"),
    }
}

fn reader_iread(o: &PyObject, method: &str, mut sizehint: isize) -> PyResult {
    if sizehint == 0 {
        return Ok(pystr::new_str(""));
    }
    let stream = snap(o)?.stream.ok_or_else(|| err!(ValueError, "stream is not set"))?;
    loop {
        let cres = if sizehint < 0 {
            ab::call_method_str(&stream, method, &[])?
        } else {
            ab::call_method_str(&stream, method, &[&int::new_int(sizehint as i64)])?
        };
        if !bytes::is_bytes(&cres) {
            raise!(TypeError, "stream function returned a non-bytes object ({})", tu_trunc(&tname(&cres)));
        }
        let data = bytes::bytes_data(&cres);
        let endoffile = data.is_empty();
        let rsize = data.len() + coder(o)?.pending_bytes.len();
        let s = decode_stateful(o, data, endoffile || sizehint < 0)?;
        let got = pystr::str_len(&s);
        if sizehint < 0 || got != 0 || rsize == 0 {
            return Ok(s);
        }
        sizehint = 1;
    }
}

fn tu_trunc(s: &str) -> String {
    s.chars().take(100).collect()
}

fn reader_read(o: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("read", a, 0, 1)?;
    reader_iread(o, "read", size_arg(a.first().copied())?)
}

fn reader_readline(o: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("readline", a, 0, 1)?;
    reader_iread(o, "readline", size_arg(a.first().copied())?)
}

fn reader_readlines(o: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("readlines", a, 0, 1)?;
    let s = reader_iread(o, "read", size_arg(a.first().copied())?)?;
    ab::call_method_str(&s, "splitlines", &[&bool_::new_bool(true)])
}

fn reader_reset(o: &PyObject) -> PyResult {
    idec_reset(o)
}

// --------------------------------------------------------------- stream writer

fn writer_iwrite(o: &PyObject, text: &PyObject) -> PyResult<()> {
    let out = encode_stateful(o, text, false)?;
    let stream = snap(o)?.stream.ok_or_else(|| err!(ValueError, "stream is not set"))?;
    ab::call_method_str(&stream, "write", &[&bytes::new_bytes(&out)])?;
    Ok(())
}

fn writer_write(o: &PyObject, s: &PyObject) -> PyResult {
    writer_iwrite(o, s)?;
    Ok(none::none_ref())
}

fn writer_writelines(o: &PyObject, lines: &PyObject) -> PyResult {
    let it = ab::get_iter(lines)?;
    while let Some(line) = ab::iter_next(&it)? {
        writer_iwrite(o, &line)?;
    }
    Ok(none::none_ref())
}

fn writer_reset(o: &PyObject) -> PyResult {
    let sn = snap(o)?;
    let mut st = sn.st;
    let (mut out, _) = encode_core(sn.codec, &mut st, &sn.pending_text, &sn.errors, true)?;
    sn.codec.imp.enc_reset(&mut st, &mut out);
    {
        let mut c = coder(o)?;
        c.st = st;
        c.pending_text.clear();
    }
    if !out.is_empty() {
        let stream = sn.stream.ok_or_else(|| err!(ValueError, "stream is not set"))?;
        ab::call_method_str(&stream, "write", &[&bytes::new_bytes(&out)])?;
    }
    Ok(none::none_ref())
}

// ===================================================================== module

fn create_codec(_m: &PyObject, _arg: &PyObject) -> PyResult {
    raise!(ValueError, "argument type invalid")
}

static INIT: std::sync::Once = std::sync::Once::new();

fn init_types() -> PyResult<()> {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        let base = Py_TPFLAGS_BASETYPE as u64;
        cc::init_state_type::<&'static CodecDef>(codec_type(), c"_multibytecodec.MultibyteCodec", 0);
        let t = &mut *codec_type();
        t.tp_methods = crate::mdefs![
            ("encode", FASTKW, codec_encode, "encode($self, /, input, errors=None)\n--\n\nReturn an encoded string version of `input'."),
            ("decode", FASTKW, codec_decode, "decode($self, /, input, errors=None)\n--\n\nDecodes 'input'."),
        ];
        res = type_ready(codec_type());
        if res.is_err() {
            return;
        }
        let errors_gs = || crate::getset_table![("errors", get_errors, set_errors; "how to treat errors")];

        cc::init_state_type::<Coder>(ienc_type(), c"_multibytecodec.MultibyteIncrementalEncoder", base);
        let t = &mut *ienc_type();
        t.tp_new = Some(ienc_new);
        t.tp_init = Some(noop_init);
        t.tp_methods = crate::mdefs![
            ("encode", FASTKW, ienc_encode, "encode($self, /, input, final=False)\n--\n\n"),
            ("getstate", NOARGS, ienc_getstate, "getstate($self, /)\n--\n\n"),
            ("setstate", O, ienc_setstate, "setstate($self, state, /)\n--\n\n"),
            ("reset", NOARGS, ienc_reset, "reset($self, /)\n--\n\n"),
        ];
        t.tp_getset = errors_gs();
        res = type_ready(ienc_type());
        if res.is_err() {
            return;
        }

        cc::init_state_type::<Coder>(idec_type(), c"_multibytecodec.MultibyteIncrementalDecoder", base);
        let t = &mut *idec_type();
        t.tp_new = Some(idec_new);
        t.tp_init = Some(noop_init);
        t.tp_methods = crate::mdefs![
            ("decode", FASTKW, idec_decode, "decode($self, /, input, final=False)\n--\n\n"),
            ("getstate", NOARGS, idec_getstate, "getstate($self, /)\n--\n\n"),
            ("setstate", O, idec_setstate, "setstate($self, state, /)\n--\n\n"),
            ("reset", NOARGS, idec_reset, "reset($self, /)\n--\n\n"),
        ];
        t.tp_getset = errors_gs();
        res = type_ready(idec_type());
        if res.is_err() {
            return;
        }

        cc::init_state_type::<Coder>(reader_type(), c"_multibytecodec.MultibyteStreamReader", base);
        let t = &mut *reader_type();
        t.tp_new = Some(reader_new);
        t.tp_init = Some(noop_init);
        t.tp_methods = crate::mdefs![
            ("read", FAST, reader_read, "read($self, sizeobj=None, /)\n--\n\n"),
            ("readline", FAST, reader_readline, "readline($self, sizeobj=None, /)\n--\n\n"),
            ("readlines", FAST, reader_readlines, "readlines($self, sizehintobj=None, /)\n--\n\n"),
            ("reset", NOARGS, reader_reset, "reset($self, /)\n--\n\n"),
        ];
        t.tp_getset = crate::getset_table![("errors", get_errors, set_errors; "how to treat errors"), ("stream", get_stream)];
        res = type_ready(reader_type());
        if res.is_err() {
            return;
        }

        cc::init_state_type::<Coder>(writer_type(), c"_multibytecodec.MultibyteStreamWriter", base);
        let t = &mut *writer_type();
        t.tp_new = Some(writer_new);
        t.tp_init = Some(noop_init);
        t.tp_methods = crate::mdefs![
            ("write", O, writer_write, "write($self, strobj, /)\n--\n\n"),
            ("writelines", O, writer_writelines, "writelines($self, lines, /)\n--\n\n"),
            ("reset", NOARGS, writer_reset, "reset($self, /)\n--\n\n"),
        ];
        t.tp_getset = crate::getset_table![("errors", get_errors, set_errors; "how to treat errors"), ("stream", get_stream)];
        res = type_ready(writer_type());
    });
    res
}

pub fn init_module() -> PyResult {
    init_types()?;
    let m = crate::types::module::new_module("_multibytecodec")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "MultibyteIncrementalEncoder", &type_ref(ienc_type()))?;
    dict::set_item_str(&d, "MultibyteIncrementalDecoder", &type_ref(idec_type()))?;
    dict::set_item_str(&d, "MultibyteStreamReader", &type_ref(reader_type()))?;
    dict::set_item_str(&d, "MultibyteStreamWriter", &type_ref(writer_type()))?;
    crate::builtins::add_methods_to_dict(&d, crate::mdefs![("__create_codec", O, create_codec)], Some(&m))?;
    Ok(m)
}
