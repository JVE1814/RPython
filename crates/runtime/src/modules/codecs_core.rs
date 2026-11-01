//! `str.encode` / `bytes.decode` entry points. The codecs themselves, the
//! error handler registry and the codec registry live in `_codecs.rs`.
use crate::errors::*;
use crate::ffi::PyObject;
use crate::modules::_codecs;
use crate::modules::text_util as tu;
use crate::types::str as pystr;
use crate::raise;

fn arg_string(o: Option<&PyObject>, default: &str, what: &str) -> PyResult<String> {
    match o {
        None => Ok(default.to_string()),
        Some(v) => {
            if !pystr::is_str(v) {
                raise!(TypeError, "{}() argument '{}' must be str, not {}", what.split(':').next().unwrap_or(""), what.split(':').nth(1).unwrap_or(""), tu::tname(v));
            }
            Ok(pystr::as_utf8(v)?.to_string())
        }
    }
}

/// An encoding name that is a str with lone surrogates (not representable as
/// a Rust string): such names go through the codec registry as objects.
fn surrogate_name(o: Option<&PyObject>) -> Option<&PyObject> {
    o.filter(|v| pystr::is_str(v) && pystr::str_data(v).iter().any(|c| (0xD800..0xE000).contains(&c)))
}

/// `str.encode(encoding='utf-8', errors='strict')`.
pub fn encode_str(s: &PyObject, enc: Option<&PyObject>, errs: Option<&PyObject>) -> PyResult {
    if let Some(name) = surrogate_name(enc) {
        let errors = arg_string(errs, "strict", "encode:errors")?;
        return _codecs::encode_text_obj(s, name, &errors);
    }
    let e = arg_string(enc, "utf-8", "encode:encoding")?;
    let errors = arg_string(errs, "strict", "encode:errors")?;
    _codecs::encode_text(s, &e, &errors)
}

/// Decode raw bytes with the given encoding and error handler.
pub fn decode_bytes(b: &[u8], enc: &str, errors: &str) -> PyResult {
    _codecs::decode_text(b, None, enc, errors)
}

/// UTF-8 decoding with an error handler name.
pub fn decode_utf8(b: &[u8], errors: &str) -> PyResult {
    _codecs::decode_text(b, None, "utf-8", errors)
}

/// `bytes.decode` / `bytearray.decode` / `str(obj, encoding, errors)`.
pub fn decode_object(o: &PyObject, enc: Option<&PyObject>, errs: Option<&PyObject>) -> PyResult {
    let name = surrogate_name(enc);
    let e = if name.is_some() { String::new() } else { arg_string(enc, "utf-8", "decode:encoding")? };
    let errors = arg_string(errs, "strict", "decode:errors")?;
    if pystr::is_str(o) {
        raise!(TypeError, "decoding str is not supported");
    }
    let data = match tu::byte_data(o) {
        Some(d) => d,
        None => raise!(TypeError, "decoding to str: need a bytes-like object, {} found", tu::tname(o)),
    };
    match name {
        Some(n) => _codecs::decode_text_obj(data.as_slice(), Some(o), n, &errors),
        None => _codecs::decode_text(data.as_slice(), Some(o), &e, &errors),
    }
}
