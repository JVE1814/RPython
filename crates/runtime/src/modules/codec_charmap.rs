//! Charmap codec (`codecs.charmap_encode/decode/build`) and the EncodingMap type.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::_codecs::{DecCtx, EncCtx, Rep};
use crate::modules::text_util::{self as tu, StrBuf};
use crate::object::*;
use crate::types::exceptions as exc;
use crate::types::str as pystr;
use crate::types::{bytes, int, none};
use std::collections::HashMap;

#[repr(C)]
pub struct EncodingMapObj {
    ob_base: PyObject,
    map: *mut HashMap<u32, u8>,
}

pub static mut ENCODING_MAP_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn emap_type() -> *mut PyTypeObject {
    &raw mut ENCODING_MAP_TYPE
}

unsafe extern "C" fn emap_dealloc(o: *mut PyObject) {
    unsafe {
        let m = &mut *(o as *mut EncodingMapObj);
        if !m.map.is_null() {
            drop(Box::from_raw(m.map));
        }
        crate::alloc::free_object(o);
    }
}

fn emap_size(o: &PyObject) -> PyResult {
    let m = unsafe { &*(o.as_ptr() as *const EncodingMapObj) };
    Ok(int::new_int(unsafe { (*m.map).len() } as i64))
}

pub fn init_types() -> PyResult<()> {
    unsafe {
        let t = &mut *emap_type();
        if !t.tp_name.is_null() {
            return Ok(());
        }
        t.tp_name = c"EncodingMap".as_ptr();
        t.tp_basicsize = std::mem::size_of::<EncodingMapObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_DISALLOW_INSTANTIATION;
        t.tp_dealloc = Some(emap_dealloc);
        t.tp_methods = crate::mdefs![("size", NOARGS, emap_size, "Return the size (in bytes) of this object")];
    }
    tu::ready_type(emap_type())
}

fn is_emap(o: &PyObject) -> bool {
    o.is_type(emap_type())
}

fn emap(o: &PyObject) -> &HashMap<u32, u8> {
    unsafe { &*(*(o.as_ptr() as *const EncodingMapObj)).map }
}

/// `codecs.charmap_build(decoding_table)`.
pub fn charmap_build(table: &PyObject) -> PyResult {
    init_types()?;
    if !pystr::is_str(table) {
        raise!(TypeError, "charmap_build() argument must be str, not {}", tu::tname(table));
    }
    let d = pystr::str_data(table);
    let mut m = HashMap::new();
    for i in 0..d.len().min(256) {
        let c = d.get(i);
        if c == 0xFFFE {
            continue;
        }
        m.entry(c).or_insert(i as u8);
    }
    unsafe {
        let op = crate::alloc::alloc_object(emap_type(), std::mem::size_of::<EncodingMapObj>());
        (*(op as *mut EncodingMapObj)).map = Box::into_raw(Box::new(m));
        Ok(PyRef::from_raw(op))
    }
}

// ------------------------------------------------------------------ decoding

enum DecMap {
    Undefined,
    Char(u32),
    Str(PyRef),
}

fn decode_lookup(mapping: &PyObject, b: u8) -> PyResult<DecMap> {
    let key = int::new_int(b as i64);
    let item = match ab::getitem(mapping, &key) {
        Ok(v) => v,
        Err(e) => {
            if exc_matches(exc::LookupError()) {
                clear_exc();
                return Ok(DecMap::Undefined);
            }
            return Err(e);
        }
    };
    if none::is_none(&item) {
        return Ok(DecMap::Undefined);
    }
    if int::is_int(&item) {
        let v = ab::as_i64(&item).unwrap_or(-1);
        if !(0..=0x10FFFF).contains(&v) {
            if crate::errors::err_occurred() {
                clear_exc();
            }
            raise!(TypeError, "character mapping must be in range(0x110000)");
        }
        if v == 0xFFFE {
            return Ok(DecMap::Undefined);
        }
        return Ok(DecMap::Char(v as u32));
    }
    if pystr::is_str(&item) {
        if pystr::str_len(&item) == 1 && pystr::str_data(&item).get(0) == 0xFFFE {
            return Ok(DecMap::Undefined);
        }
        return Ok(DecMap::Str(item));
    }
    raise!(TypeError, "character mapping must return integer, None or str")
}

/// `codecs.charmap_decode(data, errors, mapping)` (mapping None = Latin-1).
pub fn charmap_decode(ctx: &mut DecCtx, mapping: Option<&PyObject>) -> PyResult<PyRef> {
    let mapping = match mapping {
        Some(m) if !none::is_none(m) => m,
        _ => {
            return Ok(pystr::from_latin1(&ctx.data));
        }
    };
    let mut out = StrBuf::with_capacity(ctx.data.len());
    let mut i = 0;
    if pystr::is_str(mapping) {
        let table = pystr::str_data(mapping);
        loop {
            let mut err = None;
            {
                let d: &[u8] = &ctx.data;
                while i < d.len() {
                    let b = d[i] as usize;
                    let c = if b < table.len() { table.get(b) } else { 0xFFFE };
                    if c == 0xFFFE {
                        err = Some(i);
                        break;
                    }
                    out.push(c);
                    i += 1;
                }
            }
            match err {
                Some(p) => i = ctx.handle(p, p + 1, "character maps to <undefined>", &mut out)?,
                None => break,
            }
        }
        return Ok(out.finish());
    }
    loop {
        if i >= ctx.data.len() {
            break;
        }
        let b = ctx.data[i];
        match decode_lookup(mapping, b)? {
            DecMap::Char(c) => {
                out.push(c);
                i += 1;
            }
            DecMap::Str(s) => {
                out.extend_obj(&s);
                i += 1;
            }
            DecMap::Undefined => {
                i = ctx.handle(i, i + 1, "character maps to <undefined>", &mut out)?;
            }
        }
    }
    Ok(out.finish())
}

// ------------------------------------------------------------------ encoding

/// Encode one character through the mapping; Ok(false) if undefined.
fn encode_one(mapping: &PyObject, c: u32, out: &mut Vec<u8>) -> PyResult<bool> {
    if is_emap(mapping) {
        return Ok(match emap(mapping).get(&c) {
            Some(&b) => {
                out.push(b);
                true
            }
            None => false,
        });
    }
    let key = int::new_int(c as i64);
    let item = match ab::getitem(mapping, &key) {
        Ok(v) => v,
        Err(e) => {
            if exc_matches(exc::LookupError()) {
                clear_exc();
                return Ok(false);
            }
            return Err(e);
        }
    };
    if none::is_none(&item) {
        return Ok(false);
    }
    if int::is_int(&item) {
        let v = ab::as_i64(&item).unwrap_or(-1);
        if !(0..=255).contains(&v) {
            if crate::errors::err_occurred() {
                clear_exc();
            }
            raise!(TypeError, "character mapping must be in range(256)");
        }
        out.push(v as u8);
        return Ok(true);
    }
    if bytes::is_bytes(&item) {
        out.extend_from_slice(bytes::bytes_data(&item));
        return Ok(true);
    }
    raise!(TypeError, "character mapping must return integer, bytes or None, not {}", tu::trunc_chars(&tu::tname(&item), 400))
}

/// `codecs.charmap_encode(str, errors, mapping)` (mapping None = Latin-1).
pub fn charmap_encode(ctx: &mut EncCtx, mapping: Option<&PyObject>) -> PyResult<Vec<u8>> {
    let mapping = match mapping {
        Some(m) if !none::is_none(m) => m,
        _ => return crate::modules::codec_impl::encode_charlimit(ctx, 256),
    };
    let obj = ctx.obj;
    let d = pystr::str_data(obj);
    let n = d.len();
    let mut out = Vec::with_capacity(n);
    let reason = "character maps to <undefined>";
    let mut i = 0;
    while i < n {
        if encode_one(mapping, d.get(i), &mut out)? {
            i += 1;
            continue;
        }
        // collect the run of unencodable characters
        let mut j = i + 1;
        let mut scratch = Vec::new();
        while j < n {
            scratch.clear();
            if encode_one(mapping, d.get(j), &mut scratch)? {
                break;
            }
            j += 1;
        }
        let (rep, np) = ctx.handle(i, j, reason)?;
        match rep {
            Rep::Bytes(b) => out.extend_from_slice(&b),
            Rep::Str(s) => {
                for c in s {
                    if !encode_one(mapping, c, &mut out)? {
                        return Err(ctx.raise(i, j, reason));
                    }
                }
            }
        }
        i = np;
    }
    Ok(out)
}
