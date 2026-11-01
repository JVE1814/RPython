//! `_bz2`: BZ2Compressor / BZ2Decompressor on top of rpy_compress::bzip2.

use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::modules::compress_common as cc;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, dict};
use rpy_compress::bzip2::{Decoder, Encoder};

#[unsafe(no_mangle)]
pub static mut _PyBZ2_Compressor_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyBZ2_Decompressor_Type: PyTypeObject = PyTypeObject::ZERO;

fn ctype() -> *mut PyTypeObject {
    &raw mut _PyBZ2_Compressor_Type
}
fn dtype() -> *mut PyTypeObject {
    &raw mut _PyBZ2_Decompressor_Type
}

struct Comp {
    enc: Encoder,
    flushed: bool,
}

struct Decomp {
    dec: Decoder,
    eof: bool,
    needs_input: bool,
    unused: Vec<u8>,
    failed: bool,
}

fn invalid() -> PyErr {
    err!(OSError, "Invalid data stream")
}

unsafe extern "C" fn comp_new(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (v, kn) = args::kwdict_to_vectorcall(&*a, kw);
            let vr: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            if kn.is_some() && t == ctype() {
                raise!(TypeError, "BZ2Compressor() takes no keyword arguments");
            }
            args::check_pos("BZ2Compressor", &vr, 0, 1)?;
            let level = match vr.first() {
                Some(o) => cc::c_int(o)?,
                None => 9,
            };
            if !(1..=9).contains(&level) {
                raise!(ValueError, "compresslevel must be between 1 and 9");
            }
            cc::new_obj(t, Comp { enc: Encoder::new(level as u32), flushed: false })
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
        st.enc.compress(d);
        st.enc.take_output()
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
        st.enc.finish();
        st.enc.take_output()
    });
    cc::new_bytes(&out)
}

unsafe extern "C" fn decomp_new(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (v, kn) = args::kwdict_to_vectorcall(&*a, kw);
            if t == dtype() && (!v.is_empty() || kn.is_some()) {
                if kn.is_some() {
                    raise!(TypeError, "BZ2Decompressor() takes no keyword arguments");
                }
                raise!(TypeError, "BZ2Decompressor() takes no arguments");
            }
            cc::new_obj(t, Decomp { dec: Decoder::new(), eof: false, needs_input: true, unused: Vec::new(), failed: false })
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
    if st.failed {
        raise!(ValueError, "Decompressor has been closed due to a previous error");
    }
    if st.eof {
        raise!(EOFError, "End of stream already reached");
    }
    let limit = if ml < 0 { usize::MAX } else { ml as usize };
    let d = b.data();
    let r = cc::maybe_blocking(d.len().max(64 * 1024), || {
        st.dec.feed(d);
        if limit == 0 { Ok(Vec::new()) } else { st.dec.read(limit) }
    });
    let out = match r {
        Ok(v) => v,
        Err(_) => {
            st.failed = true;
            st.needs_input = false;
            return Err(invalid());
        }
    };
    if st.dec.is_done() {
        st.eof = true;
        st.needs_input = false;
        st.unused = st.dec.unused().to_vec();
    } else if st.dec.has_output() || (limit != usize::MAX && out.len() == limit) {
        st.needs_input = false;
    } else {
        st.needs_input = true;
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

static INIT: std::sync::Once = std::sync::Once::new();

pub fn init_module() -> PyResult {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        cc::init_state_type::<Comp>(ctype(), c"_bz2.BZ2Compressor", Py_TPFLAGS_BASETYPE as u64);
        let t = &mut *ctype();
        t.tp_new = Some(comp_new);
        t.tp_doc = c"BZ2Compressor(compresslevel=9, /)\n--\n\nCreate a compressor object for compressing data incrementally.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("compress", O, c_compress, "compress($self, data, /)\n--\n\nProvide data to the compressor object."),
            ("flush", NOARGS, c_flush, "flush($self, /)\n--\n\nFinish the compression process."),
            ("__reduce__", NOARGS, cc::no_pickle),
        ];
        res = type_ready(ctype());
        if res.is_err() {
            return;
        }
        cc::init_state_type::<Decomp>(dtype(), c"_bz2.BZ2Decompressor", Py_TPFLAGS_BASETYPE as u64);
        let t = &mut *dtype();
        t.tp_new = Some(decomp_new);
        t.tp_doc = c"BZ2Decompressor()\n--\n\nCreate a decompressor object for decompressing data incrementally.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("decompress", FASTKW, d_decompress, "decompress($self, /, data, max_length=-1)\n--\n\nDecompress *data*, returning uncompressed data as bytes."),
            ("__reduce__", NOARGS, cc::no_pickle),
        ];
        t.tp_getset = crate::getset_table![
            ("eof", d_eof; "True if the end-of-stream marker has been reached."),
            ("unused_data", d_unused; "Data found after the end of the compressed stream."),
            ("needs_input", d_needs_input; "True if more input is needed before more decompressed data can be produced."),
        ];
        res = type_ready(dtype());
    });
    res?;
    let m = crate::types::module::new_module("_bz2")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "BZ2Compressor", &type_ref(ctype()))?;
    dict::set_item_str(&d, "BZ2Decompressor", &type_ref(dtype()))?;
    Ok(m)
}
