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

pub fn str_repr_string(o: &PyObject) -> String {
    let d = str::str_data(o);
    let mut has_sq = false;
    let mut has_dq = false;
    for c in d.iter() {
        if c == '\'' as u32 {
            has_sq = true;
        } else if c == '"' as u32 {
            has_dq = true;
        }
    }
    let q = if has_sq && !has_dq { '"' } else { '\'' };
    let mut out = String::with_capacity(d.len() + 2);
    out.push(q);
    for c in d.iter() {
        match c {
            0x5c => out.push_str("\\\\"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            _ if c == q as u32 => {
                out.push('\\');
                out.push(q);
            }
            _ if c < 0x20 || c == 0x7f => out.push_str(&format!("\\x{:02x}", c)),
            _ if c < 0x7f => out.push(c as u8 as char),
            _ => {
                if rpy_unicode::is_printable(c) {
                    out.push(char::from_u32(c).unwrap());
                } else if c <= 0xff {
                    out.push_str(&format!("\\x{:02x}", c));
                } else if c <= 0xffff {
                    out.push_str(&format!("\\u{:04x}", c));
                } else {
                    out.push_str(&format!("\\U{:08x}", c));
                }
            }
        }
    }
    out.push(q);
    out
}

unsafe extern "C" fn str_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { str::new_str(&str_repr_string(&*o)).into_raw() }
}
unsafe extern "C" fn str_str(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        if str::is_str_exact(o) {
            return o.to_ref().into_raw();
        }
        str::from_data(str::str_data(o)).into_raw()
    }
}
unsafe extern "C" fn str_hash_slot(o: *mut PyObject) -> Py_hash_t {
    unsafe { str::str_hash(&*o) }
}
unsafe extern "C" fn str_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !str::is_str(b) || !str::is_str(a) {
            return ni();
        }
        if op == Py_EQ || op == Py_NE {
            let eq = str::str_eq(a, b);
            return bool_::new_bool(if op == Py_EQ { eq } else { !eq }).into_raw();
        }
        bool_::new_bool(cmp_result(str::str_cmp(a, b), op)).into_raw()
    }
}
unsafe extern "C" fn str_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe { str::str_len(&*o) as isize }
}
unsafe extern "C" fn str_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !str::is_str(b) {
            return into_c(Err(err!(TypeError, "can only concatenate str (not \"{}\") to str", tname(b))));
        }
        str::concat(a, b).into_raw()
    }
}
unsafe extern "C" fn str_repeat(a: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let a = &*a;
        if n <= 0 {
            return str::new_str("").into_raw();
        }
        let len = str::str_len(a);
        if len.checked_mul(n as usize).map_or(true, |t| t > isize::MAX as usize / 4) {
            return into_c(Err(err!(OverflowError, "repeated string is too long")));
        }
        let r = str::alloc_str(len * n as usize, str::max_char(a));
        let k = str::str_kind(&r);
        for i in 0..n as usize {
            str::write_into(&r, i * len, str::str_data(a), k);
        }
        if !str::str_is_ascii(a) && str::str_kind(&r) != str::str_kind(a) {
            return str::from_data(str::str_data(&r)).into_raw();
        }
        r.into_raw()
    }
}
pub fn str_find(hay: &PyObject, needle: &PyObject, start: usize, end: usize) -> Option<usize> {
    let h = str::str_data(hay);
    let n = str::str_data(needle);
    let nl = n.len();
    if nl == 0 {
        return if start <= end { Some(start) } else { None };
    }
    if end < nl || end - nl < start {
        return None;
    }
    let first = n.get(0);
    let mut i = start;
    while i + nl <= end {
        if h.get(i) == first {
            let mut ok = true;
            for j in 1..nl {
                if h.get(i + j) != n.get(j) {
                    ok = false;
                    break;
                }
            }
            if ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}
unsafe extern "C" fn str_contains(a: *mut PyObject, b: *mut PyObject) -> c_int {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !str::is_str(b) {
            let _ = err!(TypeError, "'in <string>' requires string as left operand, not {}", tname(b));
            return -1;
        }
        str_find(a, b, 0, str::str_len(a)).is_some() as c_int
    }
}
unsafe extern "C" fn str_item(a: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let a = &*a;
        let n = str::str_len(a) as isize;
        if i < 0 || i >= n {
            return into_c(Err(err!(IndexError, "string index out of range")));
        }
        let c = str::str_data(a).get(i as usize);
        str::from_codepoints(&[c]).into_raw()
    }
}
unsafe extern "C" fn str_subscript(a: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, k) = (&*a, &*k);
        if slice::is_slice(k) {
            let n = str::str_len(a) as isize;
            return into_c((|| {
                let (start, _stop, step, len) = slice::indices(k, n)?;
                let d = str::str_data(a);
                let mut v = Vec::with_capacity(len as usize);
                let mut i = start;
                for _ in 0..len {
                    v.push(d.get(i as usize));
                    i += step;
                }
                Ok(str::from_codepoints(&v))
            })());
        }
        if ab::has_index(k) {
            let mut i = match ab::index_as_ssize(k, Some(exceptions::IndexError())) {
                Ok(i) => i,
                Err(_) => return std::ptr::null_mut(),
            };
            if i < 0 {
                i += str::str_len(a) as isize;
            }
            return str_item(a.as_ptr(), i);
        }
        into_c(Err(err!(TypeError, "string indices must be integers, not '{}'", tname(k))))
    }
}
#[repr(C)]
struct StrIter {
    ob_base: PyObject,
    idx: usize,
    s: *mut PyObject,
}
static mut STR_ITER_TYPE: PyTypeObject = PyTypeObject::ZERO;
unsafe extern "C" fn str_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut STR_ITER_TYPE, std::mem::size_of::<StrIter>());
        (*(op as *mut StrIter)).s = (*o).to_ref().into_raw();
        op
    }
}
unsafe extern "C" fn str_iter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut StrIter);
        if it.s.is_null() {
            return std::ptr::null_mut();
        }
        let s = &*it.s;
        if it.idx >= str::str_len(s) {
            let p = std::mem::replace(&mut it.s, std::ptr::null_mut());
            decref(p);
            return std::ptr::null_mut();
        }
        let c = str::str_data(s).get(it.idx);
        it.idx += 1;
        str::from_codepoints(&[c]).into_raw()
    }
}
unsafe extern "C" fn simple_iter_dealloc(o: *mut PyObject) {
    unsafe {
        let it = &mut *(o as *mut StrIter);
        xdecref(it.s);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}
unsafe extern "C" fn str_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        static P: crate::args::Params = crate::args::Params::new("str", &["object", "encoding", "errors"], 0, 0);
        let r = (|| -> PyResult {
            let (all, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = all.iter().map(|r| &**r).collect();
            let [o, enc, errs] = crate::args::parse::<3>(&P, &refs, kwn.as_deref())?;
            let s = match (o, enc, errs) {
                (None, None, None) => str::new_str(""),
                (Some(o), None, None) => ab::str(o)?,
                (o, enc, errs) => {
                    let o = o.ok_or_else(|| err!(TypeError, "str() missing object argument"))?;
                    crate::modules::codecs_core::decode_object(o, enc, errs)?
                }
            };
            if tp == str::str_type() {
                return Ok(s);
            }
            crate::types::str_sub::new_str_subclass(tp, &s)
        })();
        into_c(r)
    }
}
fn str_format_m(o: &PyObject, spec: &PyObject) -> PyResult {
    format_impl::format_str(o, spec)
}
fn str_join(sep: &PyObject, it: &PyObject) -> PyResult {
    let items = ab::collect_iterable(it)?;
    let mut sb = str::StrBuilder::new();
    for (i, x) in items.iter().enumerate() {
        if !str::is_str(x) {
            raise!(TypeError, "sequence item {}: expected str instance, {} found", i, tname(x));
        }
        if i > 0 {
            sb.push_obj(sep);
        }
        sb.push_obj(x);
    }
    Ok(sb.finish())
}
fn str_encode_m(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("encode", &["encoding", "errors"], 0, 0);
    let [enc, errs] = crate::args::parse::<2>(&P, args, kw)?;
    crate::modules::codecs_core::encode_str(o, enc, errs)
}

// ============================================================== float

unsafe extern "C" fn float_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { str::new_str(&rpy_numfmt::float_repr(float::float_value(&*o))).into_raw() }
}
unsafe extern "C" fn float_hash_slot(o: *mut PyObject) -> Py_hash_t {
    unsafe { float::float_hash(&*o) }
}
fn to_double(o: &PyObject) -> PyResult<Option<f64>> {
    if float::is_float(o) {
        return Ok(Some(float::float_value(o)));
    }
    if int::is_int(o) {
        return int::to_f64(o).map(Some);
    }
    Ok(None)
}
unsafe extern "C" fn float_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        let x = float::float_value(a);
        let r = if float::is_float(b) {
            let y = float::float_value(b);
            match op {
                Py_LT => x < y,
                Py_LE => x <= y,
                Py_EQ => x == y,
                Py_NE => x != y,
                Py_GT => x > y,
                _ => x >= y,
            }
        } else if int::is_int(b) {
            if x.is_nan() {
                op == Py_NE
            } else if x.is_infinite() {
                let c = if x > 0.0 { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Less };
                cmp_result(c, op)
            } else {
                let c = rpy_bigint::conv::cmp_f64(int_methods::iref(b), x).map(|c| c.reverse()).unwrap_or(std::cmp::Ordering::Equal);
                cmp_result(c, op)
            }
        } else {
            return ni();
        };
        bool_::new_bool(r).into_raw()
    }
}
macro_rules! fbin {
    ($name:ident, |$x:ident, $y:ident| $body:expr) => {
        unsafe extern "C" fn $name(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
            unsafe {
                let (a, b) = (&*a, &*b);
                let $x = match to_double(a) {
                    Ok(Some(v)) => v,
                    Ok(None) => return ni(),
                    Err(_) => return std::ptr::null_mut(),
                };
                let $y = match to_double(b) {
                    Ok(Some(v)) => v,
                    Ok(None) => return ni(),
                    Err(_) => return std::ptr::null_mut(),
                };
                let r: PyResult = $body;
                into_c(r)
            }
        }
    };
}
fbin!(float_add, |x, y| Ok(float::new_float(x + y)));
fbin!(float_sub, |x, y| Ok(float::new_float(x - y)));
fbin!(float_mul, |x, y| Ok(float::new_float(x * y)));
fbin!(float_div, |x, y| if y == 0.0 { Err(err!(ZeroDivisionError, "division by zero")) } else { Ok(float::new_float(x / y)) });
fn float_floordiv_mod(x: f64, y: f64) -> PyResult<(f64, f64)> {
    if y == 0.0 {
        raise!(ZeroDivisionError, "division by zero");
    }
    let mut m = x % y;
    let mut d = (x - m) / y;
    if m != 0.0 {
        if (y < 0.0) != (m < 0.0) {
            m += y;
            d -= 1.0;
        }
    } else {
        m = 0.0f64.copysign(y);
    }
    let fd = if d != 0.0 {
        let f = d.floor();
        if d - f > 0.5 { f + 1.0 } else { f }
    } else {
        0.0f64.copysign(x / y)
    };
    Ok((fd, m))
}
fbin!(float_floordiv, |x, y| float_floordiv_mod(x, y).map(|(d, _)| float::new_float(d)));
fbin!(float_mod, |x, y| float_floordiv_mod(x, y).map(|(_, m)| float::new_float(m)));
fbin!(float_divmod, |x, y| float_floordiv_mod(x, y).map(|(d, m)| tuple::new_tuple(vec![float::new_float(d), float::new_float(m)])));
unsafe extern "C" fn float_pow(a: *mut PyObject, b: *mut PyObject, m: *mut PyObject) -> *mut PyObject {
    unsafe {
        if !(*m).is(none::none()) {
            return into_c(Err(err!(TypeError, "pow() 3rd argument not allowed unless all arguments are integers")));
        }
        let (a, b) = (&*a, &*b);
        let x = match to_double(a) {
            Ok(Some(v)) => v,
            Ok(None) => return ni(),
            Err(_) => return std::ptr::null_mut(),
        };
        let y = match to_double(b) {
            Ok(Some(v)) => v,
            Ok(None) => return ni(),
            Err(_) => return std::ptr::null_mut(),
        };
        into_c(crate::types::float_math::float_pow(x, y))
    }
}
unsafe extern "C" fn float_neg(a: *mut PyObject) -> *mut PyObject {
    unsafe { float::new_float(-float::float_value(&*a)).into_raw() }
}
unsafe extern "C" fn float_pos(a: *mut PyObject) -> *mut PyObject {
    unsafe { float::new_float(float::float_value(&*a)).into_raw() }
}
unsafe extern "C" fn float_abs(a: *mut PyObject) -> *mut PyObject {
    unsafe { float::new_float(float::float_value(&*a).abs()).into_raw() }
}
unsafe extern "C" fn float_bool(a: *mut PyObject) -> c_int {
    unsafe { (float::float_value(&*a) != 0.0) as c_int }
}
pub fn float_to_int(x: f64) -> PyResult {
    if x.is_nan() {
        raise!(ValueError, "cannot convert float NaN to integer");
    }
    if x.is_infinite() {
        raise!(OverflowError, "cannot convert float infinity to integer");
    }
    if x.abs() < 9.0e18 {
        return Ok(int::new_int(x.trunc() as i64));
    }
    match rpy_bigint::conv::from_f64(x) {
        Ok(b) => Ok(int_methods::from_big(b)),
        Err(e) => Err(int_methods::int_error(e)),
    }
}
unsafe extern "C" fn float_int(a: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(float_to_int(float::float_value(&*a))) }
}
unsafe extern "C" fn float_float(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = &*a;
        if float::is_float_exact(a) { a.to_ref().into_raw() } else { float::new_float(float::float_value(a)).into_raw() }
    }
}
unsafe extern "C" fn float_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if !kw.is_null() && dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "float() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "float expected at most 1 argument, got {}", a.len());
            }
            let v = match a.first() {
                None => 0.0,
                Some(x) => crate::types::float_math::number_float(x)?,
            };
            float::new_float_typed(tp, v)
        })();
        into_c(r)
    }
}
fn float_format_m(o: &PyObject, spec: &PyObject) -> PyResult {
    format_impl::format_float(float::float_value(o), spec, &tname(o))
}
fn float_is_integer(o: &PyObject) -> PyResult {
    let x = float::float_value(o);
    Ok(bool_::new_bool(x.is_finite() && x.fract() == 0.0))
}
fn float_trunc_m(o: &PyObject) -> PyResult {
    float_to_int(float::float_value(o))
}
fn float_floor_m(o: &PyObject) -> PyResult {
    float_to_int(float::float_value(o).floor())
}
fn float_ceil_m(o: &PyObject) -> PyResult {
    float_to_int(float::float_value(o).ceil())
}
fn float_round_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("__round__", args, 0, 1)?;
    let x = float::float_value(o);
    match args.first().filter(|a| !a.is(none::none())) {
        None => {
            let r = x.round();
            let r = if (x - x.trunc()).abs() == 0.5 { 2.0 * (x / 2.0).round() } else { r };
            float_to_int(r)
        }
        Some(n) => {
            let n = ab::as_i64(n)?;
            rpy_numfmt::float_round(x, n).map(float::new_float).map_err(format_impl::numfmt_error)
        }
    }
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

unsafe extern "C" fn complex_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let c = complex::complex_value(&*o);
        str::new_str(&rpy_numfmt::complex_repr(c.real, c.imag)).into_raw()
    }
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
        // ---- str
        let t = &mut *str::str_type();
        t.tp_name = c"str".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyUnicodeObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_UNICODE_SUBCLASS;
        t.tp_repr = Some(str_repr);
        t.tp_str = Some(str_str);
        t.tp_hash = Some(str_hash_slot);
        t.tp_richcompare = Some(str_richcompare);
        t.tp_iter = Some(str_iter);
        t.tp_new = Some(str_new);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"str(object='') -> str\nstr(bytes_or_buffer[, encoding[, errors]]) -> str\n\nCreate a new string object from the given object.".as_ptr();
        let s = &mut *(&raw mut STR_SEQ);
        s.sq_length = Some(str_len);
        s.sq_concat = Some(str_concat);
        s.sq_repeat = Some(str_repeat);
        s.sq_item = Some(str_item);
        s.sq_contains = Some(str_contains);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut STR_MAP);
        m.mp_length = Some(str_len);
        m.mp_subscript = Some(str_subscript);
        t.tp_as_mapping = m;
        t.tp_methods = crate::mdefs![
            ("join", O, str_join, "join($self, iterable, /)\n--\n\nConcatenate any number of strings."),
            ("__format__", O, str_format_m, "__format__($self, format_spec, /)\n--\n\nReturn a formatted version of the string as described by format_spec."),
            ("encode", FASTKW, str_encode_m, "encode($self, /, encoding='utf-8', errors='strict')\n--\n\nEncode the string using the codec registered for encoding."),
        ];
        iter_type(&raw mut STR_ITER_TYPE, c"str_ascii_iterator", str_iter_next, simple_iter_dealloc, std::mem::size_of::<StrIter>());

        // ---- float
        let t = &mut *float::float_type();
        t.tp_name = c"float".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyFloatObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        t.tp_repr = Some(float_repr);
        t.tp_hash = Some(float_hash_slot);
        t.tp_richcompare = Some(float_richcompare);
        t.tp_new = Some(float_new);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"Convert a string or number to a floating-point number, if possible.".as_ptr();
        let n = &mut *(&raw mut FLOAT_NUM);
        n.nb_add = Some(float_add);
        n.nb_subtract = Some(float_sub);
        n.nb_multiply = Some(float_mul);
        n.nb_true_divide = Some(float_div);
        n.nb_floor_divide = Some(float_floordiv);
        n.nb_remainder = Some(float_mod);
        n.nb_divmod = Some(float_divmod);
        n.nb_power = Some(float_pow);
        n.nb_negative = Some(float_neg);
        n.nb_positive = Some(float_pos);
        n.nb_absolute = Some(float_abs);
        n.nb_bool = Some(float_bool);
        n.nb_int = Some(float_int);
        n.nb_float = Some(float_float);
        t.tp_as_number = n;
        t.tp_methods = crate::mdefs![
            ("__format__", O, float_format_m),
            ("is_integer", NOARGS, float_is_integer),
            ("__trunc__", NOARGS, float_trunc_m),
            ("__floor__", NOARGS, float_floor_m),
            ("__ceil__", NOARGS, float_ceil_m),
            ("__round__", FAST, float_round_m),
        ];

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

        // ---- complex
        let t = &mut *complex::complex_type();
        t.tp_name = c"complex".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyComplexObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        t.tp_repr = Some(complex_repr);
        t.tp_getattro = Some(object_generic_getattro);
    }
}
