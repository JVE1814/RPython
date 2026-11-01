//! Minimal core slots for the fundamental builtin types (enough to boot);
//! the complete method suites live in the per-type `*_methods.rs` files.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::*;
use crate::raise;
use std::ffi::c_int;

fn ni() -> *mut PyObject {
    none::not_implemented().to_ref().into_raw()
}

fn cmp_result(c: std::cmp::Ordering, op: c_int) -> bool {
    use std::cmp::Ordering::*;
    match op {
        Py_LT => c == Less,
        Py_LE => c != Greater,
        Py_EQ => c == Equal,
        Py_NE => c != Equal,
        Py_GT => c == Greater,
        _ => c != Less,
    }
}

// ============================================================== str
// (moved to str_methods.rs)

unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

// ============================================================== bytes / complex / slice / bytearray

unsafe extern "C" fn bytes_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { str::new_str(&bytes_repr_string(bytes::bytes_data(&*o), "b", "")).into_raw() }
}
pub fn bytes_repr_string(b: &[u8], prefix: &str, suffix: &str) -> String {
    let has_sq = b.contains(&b'\'');
    let has_dq = b.contains(&b'"');
    let q = if has_sq && !has_dq { '"' } else { '\'' };
    let mut out = String::with_capacity(b.len() + 3);
    out.push_str(prefix);
    out.push(q);
    for &c in b {
        match c {
            b'\\' => out.push_str("\\\\"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            _ if c as char == q => {
                out.push('\\');
                out.push(q);
            }
            0x20..=0x7e => out.push(c as char),
            _ => out.push_str(&format!("\\x{:02x}", c)),
        }
    }
    out.push(q);
    out.push_str(suffix);
    out
}
unsafe extern "C" fn bytes_hash_slot(o: *mut PyObject) -> Py_hash_t {
    unsafe { bytes::bytes_hash(&*o) }
}
unsafe extern "C" fn bytes_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !bytes::is_bytes(b) {
            if (op == Py_EQ || op == Py_NE) && str::is_str(b) && crate::modules::sys::bytes_warning() > 0 {
                if crate::modules::warnings::warn(exceptions::BytesWarning(), "Comparison between bytes and string", 1).is_err() {
                    return std::ptr::null_mut();
                }
            }
            return ni();
        }
        bool_::new_bool(cmp_result(bytes::bytes_data(a).cmp(bytes::bytes_data(b)), op)).into_raw()
    }
}
unsafe extern "C" fn bytes_len_slot(o: *mut PyObject) -> Py_ssize_t {
    unsafe { var_size(&*o) }
}
unsafe extern "C" fn bytes_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (o, k) = (&*o, &*k);
        let d = bytes::bytes_data(o);
        if slice::is_slice(k) {
            return into_c((|| {
                let (start, _s, step, len) = slice::indices(k, d.len() as isize)?;
                let mut v = Vec::with_capacity(len as usize);
                let mut i = start;
                for _ in 0..len {
                    v.push(d[i as usize]);
                    i += step;
                }
                Ok(bytes::new_bytes(&v))
            })());
        }
        if ab::has_index(k) {
            return into_c((|| {
                let mut i = ab::index_as_ssize(k, Some(exceptions::IndexError()))?;
                if i < 0 {
                    i += d.len() as isize;
                }
                if i < 0 || i as usize >= d.len() {
                    raise!(IndexError, "index out of range");
                }
                Ok(int::new_int(d[i as usize] as i64))
            })());
        }
        into_c(Err(err!(TypeError, "byte indices must be integers or slices, not {}", tname(k))))
    }
}
unsafe extern "C" fn bytes_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        match bytes::bytes_like_data(b) {
            Some(bd) => {
                let mut v = bytes::bytes_data(a).to_vec();
                v.extend_from_slice(&bd);
                bytes::new_bytes(&v).into_raw()
            }
            None => into_c(Err(err!(TypeError, "can't concat {} to {}", tname(b), tname(a)))),
        }
    }
}
unsafe extern "C" fn bytes_getbuffer(o: *mut PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    unsafe {
        let _ = flags;
        let v = &mut *view;
        v.buf = bytes::bytes_ptr(&*o) as *mut _;
        v.obj = (*o).to_ref().into_raw();
        v.len = var_size(&*o);
        v.itemsize = 1;
        v.readonly = 1;
        v.ndim = 1;
        v.format = c"B".as_ptr() as *mut _;
        v.shape = std::ptr::null_mut();
        v.strides = std::ptr::null_mut();
        v.suboffsets = std::ptr::null_mut();
        v.internal = std::ptr::null_mut();
        0
    }
}
unsafe extern "C" fn bytes_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let _ = kw;
            let a = tuple::tuple_items(&*args);
            let data: Vec<u8> = match a.first() {
                None => Vec::new(),
                Some(x) if str::is_str(x) => {
                    let enc = a.get(1).copied();
                    let errs = a.get(2).copied();
                    let r = crate::modules::codecs_core::encode_str(x, enc, errs)?;
                    bytes::bytes_data(&r).to_vec()
                }
                Some(x) if int::is_int(x) => {
                    let n = ab::as_i64(x)?;
                    if n < 0 {
                        raise!(ValueError, "negative count");
                    }
                    vec![0u8; n as usize]
                }
                Some(x) => match bytes::bytes_like_data(x) {
                    Some(d) => d,
                    None => {
                        let mut v = Vec::new();
                        for it in ab::collect_iterable(x)? {
                            let n = ab::as_i64(&it)?;
                            if !(0..256).contains(&n) {
                                raise!(ValueError, "bytes must be in range(0, 256)");
                            }
                            v.push(n as u8);
                        }
                        v
                    }
                },
            };
            if tp == bytes::bytes_type() {
                return Ok(bytes::new_bytes(&data));
            }
            let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
            let op = alloc(tp, data.len() as isize + 1);
            if op.is_null() {
                return Err(PyErr);
            }
            set_var_size(op, data.len() as isize);
            (*(op as *mut PyBytesObject)).ob_shash = -1;
            std::ptr::copy_nonoverlapping(data.as_ptr(), bytes::bytes_ptr(&*op), data.len());
            Ok(PyRef::from_raw(op))
        })();
        into_c(r)
    }
}
fn bytes_decode_m(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("decode", &["encoding", "errors"], 0, 0);
    let [enc, errs] = crate::args::parse::<2>(&P, args, kw)?;
    crate::modules::codecs_core::decode_object(o, enc, errs)
}


// ============================================================== registration

static mut STR_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut STR_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut FLOAT_NUM: PyNumberMethods = PyNumberMethods::ZERO;
static mut BYTES_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut BYTES_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut BYTES_BUF: PyBufferProcs = PyBufferProcs::ZERO;

fn iter_type(t: *mut PyTypeObject, name: &'static std::ffi::CStr, next: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject, dealloc: unsafe extern "C" fn(*mut PyObject), size: usize) {
    unsafe {
        let t = &mut *t;
        t.tp_name = name.as_ptr();
        t.tp_basicsize = size as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_dealloc = Some(dealloc);
        t.tp_iter = Some(self_iter);
        t.tp_iternext = Some(next);
        t.tp_getattro = Some(object_generic_getattro);
    }
}

pub fn init_types() {
    unsafe {
        // ---- str (slots and methods live in str_methods.rs)
        crate::types::str_methods::init_types();

        // ---- bytes
        let t = &mut *bytes::bytes_type();
        t.tp_name = c"bytes".as_ptr();
        t.tp_basicsize = std::mem::offset_of!(PyBytesObject, ob_sval) as isize;
        t.tp_itemsize = 1;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_BYTES_SUBCLASS;
        t.tp_repr = Some(bytes_repr);
        t.tp_hash = Some(bytes_hash_slot);
        t.tp_richcompare = Some(bytes_richcompare);
        t.tp_new = Some(bytes_new);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_alloc = Some(generic_alloc);
        let s = &mut *(&raw mut BYTES_SEQ);
        s.sq_length = Some(bytes_len_slot);
        s.sq_concat = Some(bytes_concat);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut BYTES_MAP);
        m.mp_length = Some(bytes_len_slot);
        m.mp_subscript = Some(bytes_subscript);
        t.tp_as_mapping = m;
        let b = &mut *(&raw mut BYTES_BUF);
        b.bf_getbuffer = Some(bytes_getbuffer);
        t.tp_as_buffer = b;
        t.tp_methods = crate::mdefs![("decode", FASTKW, bytes_decode_m)];

        // ---- bytearray (placeholder type)
        let t = &mut *bytearray::bytearray_type();
        t.tp_name = c"bytearray".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyByteArrayObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        t.tp_getattro = Some(object_generic_getattro);

    }
}
