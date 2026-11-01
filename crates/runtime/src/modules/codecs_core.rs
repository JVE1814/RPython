//! Built-in codec fast paths (utf-8, ascii, latin-1) used by str.encode / bytes.decode.
//! The full codec registry lives in the `_codecs` module (pending).
use crate::errors::*;
use crate::ffi::PyObject;
use crate::types::{bytes, str as pystr};
use crate::raise;

fn norm(enc: &str) -> String {
    enc.to_ascii_lowercase().replace('-', "_").replace(' ', "_")
}

pub fn encode_str(s: &PyObject, enc: Option<&PyObject>, errs: Option<&PyObject>) -> PyResult {
    let e = enc.map(|e| pystr::to_string_lossy(e)).unwrap_or_else(|| "utf-8".into());
    let errors = errs.map(|e| pystr::to_string_lossy(e)).unwrap_or_else(|| "strict".into());
    let n = norm(&e);
    let d = pystr::str_data(s);
    match n.as_str() {
        "utf_8" | "utf8" => {
            let mut out = Vec::with_capacity(d.len());
            for (i, c) in d.iter().enumerate() {
                match char::from_u32(c) {
                    Some(ch) => {
                        let mut b = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
                    }
                    None => {
                        if errors == "surrogateescape" && (0xdc80..0xdd00).contains(&c) {
                            out.push((c - 0xdc00) as u8);
                        } else if errors == "surrogatepass" {
                            out.extend_from_slice(&[(0xe0 | (c >> 12)) as u8, (0x80 | ((c >> 6) & 0x3f)) as u8, (0x80 | (c & 0x3f)) as u8]);
                        } else if errors == "ignore" {
                        } else if errors == "replace" {
                            out.push(b'?');
                        } else {
                            return Err(crate::types::exceptions::unicode_encode_error("utf-8", s, i, i + 1, "surrogates not allowed"));
                        }
                    }
                }
            }
            Ok(bytes::new_bytes(&out))
        }
        "ascii" | "us_ascii" | "latin_1" | "latin1" | "iso_8859_1" | "iso8859_1" => {
            let limit = if n.contains("ascii") { 128 } else { 256 };
            let name = if limit == 128 { "ascii" } else { "latin-1" };
            let mut out = Vec::with_capacity(d.len());
            for (i, c) in d.iter().enumerate() {
                if c < limit {
                    out.push(c as u8);
                } else if errors == "ignore" {
                } else if errors == "replace" {
                    out.push(b'?');
                } else if errors == "surrogateescape" && (0xdc80..0xdd00).contains(&c) {
                    out.push((c - 0xdc00) as u8);
                } else {
                    return Err(crate::types::exceptions::unicode_encode_error(
                        name,
                        s,
                        i,
                        i + 1,
                        &format!("ordinal not in range({})", limit),
                    ));
                }
            }
            Ok(bytes::new_bytes(&out))
        }
        _ => crate::modules::codecs_registry::encode(s, &e, &errors),
    }
}

pub fn decode_bytes(b: &[u8], enc: &str, errors: &str) -> PyResult {
    let n = norm(enc);
    match n.as_str() {
        "utf_8" | "utf8" => decode_utf8(b, errors),
        "ascii" | "us_ascii" => {
            let mut out = Vec::with_capacity(b.len());
            for (i, &c) in b.iter().enumerate() {
                if c < 128 {
                    out.push(c as u32);
                } else if errors == "ignore" {
                } else if errors == "replace" {
                    out.push(0xfffd);
                } else if errors == "surrogateescape" {
                    out.push(0xdc00 + c as u32);
                } else {
                    return Err(crate::types::exceptions::unicode_decode_error("ascii", b, i, i + 1, "ordinal not in range(128)"));
                }
            }
            Ok(pystr::from_codepoints(&out))
        }
        "latin_1" | "latin1" | "iso_8859_1" | "iso8859_1" => Ok(pystr::from_latin1(b)),
        _ => {
            let bo = bytes::new_bytes(b);
            crate::modules::codecs_registry::decode(&bo, enc, errors)
        }
    }
}

pub fn decode_utf8(b: &[u8], errors: &str) -> PyResult {
    if let Ok(s) = std::str::from_utf8(b) {
        return Ok(pystr::new_str(s));
    }
    let mut out: Vec<u32> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match std::str::from_utf8(&b[i..]) {
            Ok(s) => {
                out.extend(s.chars().map(|c| c as u32));
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                out.extend(std::str::from_utf8(&b[i..i + good]).unwrap().chars().map(|c| c as u32));
                i += good;
                let bad = e.error_len().unwrap_or(b.len() - i);
                let (reason, end) = match e.error_len() {
                    None => ("unexpected end of data", b.len()),
                    Some(_) => {
                        let c = b[i];
                        if (0x80..0xc2).contains(&c) || c > 0xf4 {
                            ("invalid start byte", i + 1)
                        } else {
                            ("invalid continuation byte", i + bad)
                        }
                    }
                };
                match errors {
                    "ignore" => {}
                    "replace" => out.push(0xfffd),
                    "surrogateescape" => {
                        for k in i..i + bad {
                            out.push(0xdc00 + b[k] as u32);
                        }
                    }
                    "backslashreplace" => {
                        for k in i..i + bad {
                            for ch in format!("\\x{:02x}", b[k]).chars() {
                                out.push(ch as u32);
                            }
                        }
                    }
                    _ => return Err(crate::types::exceptions::unicode_decode_error("utf-8", b, i, end, reason)),
                }
                i += bad;
            }
        }
    }
    Ok(pystr::from_codepoints(&out))
}

pub fn decode_object(o: &PyObject, enc: Option<&PyObject>, errs: Option<&PyObject>) -> PyResult {
    let e = enc.map(|e| pystr::to_string_lossy(e)).unwrap_or_else(|| "utf-8".into());
    let errors = errs.map(|e| pystr::to_string_lossy(e)).unwrap_or_else(|| "strict".into());
    if pystr::is_str(o) {
        raise!(TypeError, "decoding str is not supported");
    }
    let data = match bytes::bytes_like_data(o) {
        Some(d) => d,
        None => raise!(TypeError, "decoding to str: need a bytes-like object, {} found", crate::types::typeobject::tname(o)),
    };
    decode_bytes(&data, &e, &errors)
}
