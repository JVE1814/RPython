//! codecs.h: codec registry access (delegates to the `_codecs` / `codecs` modules).

use super::*;
use crate::abstract_ as ab;
use crate::types::{none, str as pystr, tuple};

fn codecs_mod() -> PyResult {
    crate::import::import_absolute("_codecs")
}
unsafe fn call_codecs(name: &str, args: &[&PyObject]) -> PyResult {
    let m = codecs_mod()?;
    let f = ab::getattr_str(&m, name)?;
    ab::call(&f, args)
}
unsafe fn opt_str(p: *const c_char) -> PyResult {
    unsafe { if p.is_null() { Ok(none::none_ref()) } else { cstr_obj(p) } }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_Register(search: *mut PyObject) -> c_int {
    unsafe { ret_i(call_codecs("register", &[&*search]).map(|_| ())) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_Unregister(search: *mut PyObject) -> c_int {
    unsafe { ret_i(call_codecs("unregister", &[&*search]).map(|_| ())) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_KnownEncoding(enc: *const c_char) -> c_int {
    unsafe {
        match cstr_obj(enc).and_then(|e| call_codecs("lookup", &[&e])) {
            Ok(_) => 1,
            Err(_) => {
                clear_exc();
                0
            }
        }
    }
}
/// The encoding name argument (NULL is a bad argument).
unsafe fn enc_name(enc: *const c_char) -> PyResult {
    unsafe {
        if enc.is_null() {
            return Err(type_error("bad argument type for built-in operation"));
        }
        cstr_obj(enc)
    }
}
/// The errors argument (NULL means strict).
unsafe fn errors_name(errors: *const c_char) -> PyResult {
    unsafe { if errors.is_null() { Ok(pystr::new_str("strict")) } else { cstr_obj(errors) } }
}
unsafe fn lookup(enc: *const c_char) -> PyResult {
    unsafe {
        let e = enc_name(enc)?;
        call_codecs("lookup", &[&e])
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_Encode(o: *mut PyObject, enc: *const c_char, errors: *const c_char) -> *mut PyObject {
    unsafe {
        ret((|| {
            let o = super::nn(o).map_err(|_| { clear_exc(); type_error("bad argument type for built-in operation") })?;
            call_codecs("encode", &[o, &*enc_name(enc)?, &*errors_name(errors)?])
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_Decode(o: *mut PyObject, enc: *const c_char, errors: *const c_char) -> *mut PyObject {
    unsafe {
        ret((|| {
            let o = super::nn(o).map_err(|_| { clear_exc(); type_error("bad argument type for built-in operation") })?;
            call_codecs("decode", &[o, &*enc_name(enc)?, &*errors_name(errors)?])
        })())
    }
}
unsafe fn codec_item(enc: *const c_char, idx: usize) -> *mut PyObject {
    unsafe {
        ret((|| {
            let ci = lookup(enc)?;
            // a CodecInfo is a 4-tuple (encode, decode, reader, writer) with attributes
            let attr = ["encode", "decode", "incrementalencoder", "incrementaldecoder", "streamreader", "streamwriter"][idx];
            if !tuple::is_tuple(&ci) || crate::types::typeobject::is_heap_type(ci.type_ptr()) {
                return ab::getattr_str(&ci, attr);
            }
            let ti = [0usize, 1, usize::MAX, usize::MAX, 2, 3][idx];
            if ti < tuple::tuple_len(&ci) {
                return Ok(tuple::tuple_get(&ci, ti).to_ref());
            }
            Err(crate::err!(AttributeError, "codec has no {}", attr))
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_Encoder(enc: *const c_char) -> *mut PyObject {
    unsafe { codec_item(enc, 0) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_Decoder(enc: *const c_char) -> *mut PyObject {
    unsafe { codec_item(enc, 1) }
}
unsafe fn codec_factory(enc: *const c_char, idx: usize, args: &[&PyObject]) -> *mut PyObject {
    unsafe {
        let f = codec_item(enc, idx);
        if f.is_null() {
            return f;
        }
        let f = PyRef::from_raw(f);
        ret(ab::call(&f, args))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_IncrementalEncoder(enc: *const c_char, errors: *const c_char) -> *mut PyObject {
    unsafe {
        match opt_str(errors) {
            Ok(e) if !none::is_none(&e) => codec_factory(enc, 2, &[&e]),
            Ok(_) => codec_factory(enc, 2, &[]),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_IncrementalDecoder(enc: *const c_char, errors: *const c_char) -> *mut PyObject {
    unsafe {
        match opt_str(errors) {
            Ok(e) if !none::is_none(&e) => codec_factory(enc, 3, &[&e]),
            Ok(_) => codec_factory(enc, 3, &[]),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_StreamReader(enc: *const c_char, stream: *mut PyObject, errors: *const c_char) -> *mut PyObject {
    unsafe {
        match opt_str(errors) {
            Ok(e) => codec_factory(enc, 4, &[&*stream, &e]),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_StreamWriter(enc: *const c_char, stream: *mut PyObject, errors: *const c_char) -> *mut PyObject {
    unsafe {
        match opt_str(errors) {
            Ok(e) => codec_factory(enc, 5, &[&*stream, &e]),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_RegisterError(name: *const c_char, error: *mut PyObject) -> c_int {
    unsafe { ret_i((|| call_codecs("register_error", &[&*cstr_obj(name)?, &*error]).map(|_| ()))()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_LookupError(name: *const c_char) -> *mut PyObject {
    unsafe {
        let n = if name.is_null() { pystr::new_str("strict") } else { match cstr_obj(name) { Ok(n) => n, Err(_) => return std::ptr::null_mut() } };
        ret(call_codecs("lookup_error", &[&n]))
    }
}
unsafe fn handler(name: &str, exc: *mut PyObject) -> *mut PyObject {
    unsafe {
        ret((|| {
            let h = call_codecs("lookup_error", &[&pystr::new_str(name)])?;
            ab::call(&h, &[&*exc])
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_StrictErrors(exc: *mut PyObject) -> *mut PyObject {
    unsafe {
        if crate::types::exceptions::is_exception_instance(&*exc) {
            set_exc((*exc).to_ref());
        } else {
            let _ = type_error("codec must pass exception instance");
        }
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_IgnoreErrors(exc: *mut PyObject) -> *mut PyObject {
    unsafe { handler("ignore", exc) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_ReplaceErrors(exc: *mut PyObject) -> *mut PyObject {
    unsafe { handler("replace", exc) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_XMLCharRefReplaceErrors(exc: *mut PyObject) -> *mut PyObject {
    unsafe { handler("xmlcharrefreplace", exc) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_BackslashReplaceErrors(exc: *mut PyObject) -> *mut PyObject {
    unsafe { handler("backslashreplace", exc) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCodec_NameReplaceErrors(exc: *mut PyObject) -> *mut PyObject {
    unsafe { handler("namereplace", exc) }
}
