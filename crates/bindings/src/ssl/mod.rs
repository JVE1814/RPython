//! `_ssl`: TLS/SSL wrapper binding OpenSSL 3 (libssl/libcrypto via dlopen).

pub mod context;
pub mod ffi;
pub mod socket;

use crate::capi;
use crate::ctypes::{call, cstring, from_c, getattr_opt, is_bytes, is_none, is_str, new_int, new_str, none, str_of, tname};
use crate::{cr, cs, sl};
use ffi::*;
use rpy_runtime::errors::*;
use rpy_runtime::ffi::*;
use rpy_runtime::modules::fncommon::{self as fc, Trav};
use rpy_runtime::object::*;
use rpy_runtime::types::{bytes, dict, tuple};
use rpy_runtime::{err, raise};
use std::ffi::{CStr, CString, c_char, c_int, c_long, c_void};

// ------------------------------------------------------------------ module state

pub struct State {
    pub ssl_error: PyRef,
    pub cert_verify_error: PyRef,
    pub zero_return_error: PyRef,
    pub want_read_error: PyRef,
    pub want_write_error: PyRef,
    pub syscall_error: PyRef,
    pub eof_error: PyRef,
}
unsafe impl Send for State {}
unsafe impl Sync for State {}

static STATE: std::sync::OnceLock<State> = std::sync::OnceLock::new();

pub fn st() -> &'static State {
    STATE.get().expect("_ssl not initialised")
}

pub fn cstr(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() } }
}

// ------------------------------------------------------------------ errors

fn err_get_lib(e: u64) -> i32 {
    if e & 0x8000_0000 != 0 { 2 } else { ((e >> 23) & 0xff) as i32 }
}
fn err_get_reason(e: u64) -> i32 {
    if e & 0x8000_0000 != 0 { (e & 0x7fff_ffff) as i32 } else { (e & 0x7f_ffff) as i32 }
}

pub fn lib_name(lib: i32) -> Option<&'static str> {
    Some(match lib {
        1 => "NONE",
        2 => "SYS",
        3 => "BN",
        4 => "RSA",
        5 => "DH",
        6 => "EVP",
        7 => "BUF",
        8 => "OBJ",
        9 => "PEM",
        10 => "DSA",
        11 => "X509",
        13 => "ASN1",
        14 => "CONF",
        15 => "CRYPTO",
        16 => "EC",
        20 => "SSL",
        32 => "BIO",
        33 => "PKCS7",
        34 => "X509V3",
        35 => "PKCS12",
        36 => "RAND",
        37 => "DSO",
        38 => "ENGINE",
        39 => "OCSP",
        40 => "UI",
        41 => "COMP",
        44 => "OSSL_STORE",
        46 => "CMS",
        52 => "KDF",
        57 => "PROV",
        59 => "OSSL_ENCODER",
        60 => "OSSL_DECODER",
        _ => return None,
    })
}

/// Mnemonic of an OpenSSL reason ("certificate verify failed" -> "CERTIFICATE_VERIFY_FAILED").
pub fn reason_mnemonic(e: u64) -> Option<String> {
    let p = cr!(ERR_reason_error_string(e as _));
    if p.is_null() {
        return None;
    }
    let s = cstr(p);
    Some(s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect())
}

/// Create an `SSLError`-family exception instance with library/reason attributes.
pub fn make_ssl_error(exc: &PyObject, errno: i32, msg: &str, lib: Option<&str>, reason: Option<&str>) -> PyResult {
    let e = rpy_runtime::types::exceptions::make_exception(exc.as_ptr() as *mut PyTypeObject, &[new_int(errno as i64), new_str(msg)])?;
    let lv = lib.map(new_str).unwrap_or_else(none);
    let rv = reason.map(new_str).unwrap_or_else(none);
    rpy_runtime::abstract_::setattr(&e, &new_str("library"), &lv)?;
    rpy_runtime::abstract_::setattr(&e, &new_str("reason"), &rv)?;
    Ok(e)
}

/// Raise an SSLError from the OpenSSL error queue (or `fallback` text).
pub fn raise_from_queue(exc: Option<&PyObject>, fallback: &str) -> PyErr {
    let e = cr!(ERR_peek_last_error()) as u64;
    cr!(ERR_clear_error());
    raise_errcode(exc, e, 0, fallback, None)
}

pub fn raise_errcode(exc: Option<&PyObject>, e: u64, ssl_errno: i32, fallback: &str, verify: Option<(i64, String)>) -> PyErr {
    let s = st();
    let mut exc_type = exc.map(|x| x.to_ref()).unwrap_or_else(|| s.ssl_error.clone());
    let (lib, reason, errstr) = if e != 0 {
        let lib = lib_name(err_get_lib(e));
        let reason = reason_mnemonic(e);
        let es = cstr(cr!(ERR_reason_error_string(e as _)));
        (lib, reason, if es.is_empty() { fallback.to_string() } else { es })
    } else {
        (None, None, fallback.to_string())
    };
    let mut msg = match (&lib, &reason) {
        (Some(l), Some(r)) => format!("[{}: {}] {}", l, r, errstr),
        (None, Some(r)) => format!("[{}] {}", r, errstr),
        _ => errstr.clone(),
    };
    if let Some((code, text)) = &verify {
        exc_type = s.cert_verify_error.clone();
        msg = format!("{}: {}", msg, text);
        let _ = code;
    }
    msg.push_str(" (_ssl.c:1000)");
    match make_ssl_error(&exc_type, ssl_errno, &msg, lib, reason.as_deref()) {
        Ok(ex) => {
            if let Some((code, text)) = verify {
                let _ = rpy_runtime::abstract_::setattr(&ex, &new_str("verify_code"), &new_int(code));
                let _ = rpy_runtime::abstract_::setattr(&ex, &new_str("verify_message"), &new_str(&text));
            }
            rpy_runtime::errors::set_exc(ex);
        }
        Err(_) => {}
    }
    PyErr
}

/// SSLError(errno, msg) without OpenSSL details.
pub fn raise_simple(exc: &PyObject, errno: i32, msg: &str) -> PyErr {
    if let Ok(e) = make_ssl_error(exc, errno, msg, None, None) {
        rpy_runtime::errors::set_exc(e);
    }
    PyErr
}

unsafe extern "C" fn ssl_error_str(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let e = &*o;
        let args = rpy_runtime::types::exceptions::exc_args(e);
        if tuple::tuple_len(&args) == 2 && is_str(tuple::tuple_get(&args, 1)) {
            return tuple::tuple_get(&args, 1).to_ref().into_raw();
        }
        if tuple::tuple_len(&args) == 1 {
            return into_c(rpy_runtime::abstract_::str(tuple::tuple_get(&args, 0)));
        }
        into_c(rpy_runtime::abstract_::str(&args))
    }
}

fn new_exc(name: &str, bases: PyRef, doc: Option<&str>) -> PyResult {
    let c = cstring(name);
    let d = dict::new_dict();
    if let Some(doc) = doc {
        dict::set_item_str(&d, "__doc__", &new_str(doc))?;
    }
    let r = from_c(unsafe { capi::PyErr_NewException(c.as_ptr(), bases.as_ptr(), d.as_ptr()) })?;
    unsafe {
        let t = r.as_ptr() as *mut PyTypeObject;
        (*t).tp_str = Some(ssl_error_str);
        (*t).tp_flags |= Py_TPFLAGS_IMMUTABLETYPE;
        capi::PyType_Modified(t);
    }
    Ok(r)
}

// ------------------------------------------------------------------ certificates

pub fn x509_name_to_py(name: *mut c_void) -> PyResult {
    let n = cr!(X509_NAME_entry_count(name));
    let mut rdns: Vec<PyRef> = Vec::new();
    let mut cur: Vec<PyRef> = Vec::new();
    let mut last_set = -1;
    for i in 0..n {
        let entry = cr!(X509_NAME_get_entry(name, i));
        let set = cr!(X509_NAME_ENTRY_set(entry));
        if set != last_set && !cur.is_empty() {
            rdns.push(tuple::new_tuple(std::mem::take(&mut cur)));
        }
        last_set = set;
        let obj = cr!(X509_NAME_ENTRY_get_object(entry));
        let data = cr!(X509_NAME_ENTRY_get_data(entry));
        let key = obj_name(obj)?;
        let val = asn1_string_to_py(data)?;
        cur.push(tuple::new_tuple(vec![key, val]));
    }
    if !cur.is_empty() {
        rdns.push(tuple::new_tuple(cur));
    }
    Ok(tuple::new_tuple(rdns))
}

fn obj_name(obj: *mut c_void) -> PyResult {
    let nid = cr!(OBJ_obj2nid(obj));
    if nid != 0 {
        let ln = cr!(OBJ_nid2ln(nid));
        if !ln.is_null() {
            return Ok(new_str(&cstr(ln)));
        }
    }
    let mut buf = [0 as c_char; 256];
    let n = cr!(OBJ_obj2txt(buf.as_mut_ptr(), 256, obj, 0));
    if n < 0 {
        return Err(raise_from_queue(None, "could not decode object"));
    }
    Ok(new_str(&cstr(buf.as_ptr())))
}

fn asn1_string_to_py(s: *mut c_void) -> PyResult {
    let mut out: *mut u8 = std::ptr::null_mut();
    let n = cr!(ASN1_STRING_to_UTF8(&mut out, s));
    if n < 0 {
        return Err(raise_from_queue(None, "could not convert string"));
    }
    let r = from_c(unsafe { capi::PyUnicode_FromStringAndSize(out as *const c_char, n as isize) });
    cr!(CRYPTO_free(out as *mut c_void, cs!("_ssl"), 0));
    r
}

fn asn1_raw(s: *mut c_void) -> Vec<u8> {
    let n = cr!(ASN1_STRING_length(s));
    let p = cr!(ASN1_STRING_get0_data(s));
    if p.is_null() || n <= 0 {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(p, n as usize) }.to_vec()
}

fn bio_to_string(f: impl FnOnce(*mut c_void) -> c_int) -> PyResult<String> {
    let b = cr!(BIO_new(cr!(BIO_s_mem())));
    if b.is_null() {
        return Err(raise_from_queue(None, "failed to allocate BIO"));
    }
    f(b);
    let mut buf = vec![0u8; 512];
    let n = cr!(BIO_read(b, buf.as_mut_ptr() as *mut c_void, 512));
    cr!(BIO_free(b));
    Ok(String::from_utf8_lossy(&buf[..n.max(0) as usize]).into_owned())
}

/// GENERAL_NAME layout: { int type; void *d; }
#[repr(C)]
struct GeneralName {
    type_: c_int,
    d: *mut c_void,
}

fn general_name_to_py(g: &GeneralName) -> PyResult<PyRef> {
    let pair = |k: &str, v: PyRef| tuple::new_tuple(vec![new_str(k), v]);
    Ok(match g.type_ {
        1 => pair("email", new_str(&String::from_utf8_lossy(&asn1_raw(g.d)))),
        2 => pair("DNS", new_str(&String::from_utf8_lossy(&asn1_raw(g.d)))),
        6 => pair("URI", new_str(&String::from_utf8_lossy(&asn1_raw(g.d)))),
        4 => pair("DirName", x509_name_to_py(g.d)?),
        7 => {
            let raw = asn1_raw(g.d);
            let s = if raw.len() == 4 {
                format!("{}.{}.{}.{}", raw[0], raw[1], raw[2], raw[3])
            } else if raw.len() == 16 {
                let parts: Vec<String> = raw.chunks(2).map(|c| format!("{:X}", ((c[0] as u16) << 8) | c[1] as u16)).collect();
                parts.join(":")
            } else {
                "<invalid>".to_string()
            };
            pair("IP Address", new_str(&s))
        }
        8 => {
            let mut buf = [0 as c_char; 256];
            cr!(OBJ_obj2txt(buf.as_mut_ptr(), 256, g.d, 0));
            pair("Registered ID", new_str(&cstr(buf.as_ptr())))
        }
        0 => pair("othername", new_str("<unsupported>")),
        3 => pair("X400Name", new_str("<unsupported>")),
        5 => pair("EdiPartyName", new_str("<unsupported>")),
        _ => pair("unknown", new_str("<unsupported>")),
    })
}

fn general_names_to_py(names: *mut c_void) -> PyResult<Vec<PyRef>> {
    let n = cr!(OPENSSL_sk_num(names));
    let mut out = Vec::new();
    for i in 0..n {
        let g = cr!(OPENSSL_sk_value(names, i)) as *const GeneralName;
        out.push(general_name_to_py(unsafe { &*g })?);
    }
    Ok(out)
}

/// Decode a certificate into the dict returned by getpeercert().
pub fn decode_certificate(x: *mut c_void) -> PyResult {
    let d = dict::new_dict();
    let subject = x509_name_to_py(cr!(X509_get_subject_name(x)))?;
    dict::set_item_str(&d, "subject", &subject)?;
    let issuer = x509_name_to_py(cr!(X509_get_issuer_name(x)))?;
    dict::set_item_str(&d, "issuer", &issuer)?;
    dict::set_item_str(&d, "version", &new_int(cr!(X509_get_version(x)) as i64 + 1))?;
    // serial number
    let bn = cr!(ASN1_INTEGER_to_BN(cr!(X509_get_serialNumber(x)), std::ptr::null_mut()));
    if !bn.is_null() {
        let hex = cr!(BN_bn2hex(bn));
        let s = cstr(hex);
        cr!(CRYPTO_free(hex as *mut c_void, cs!("_ssl"), 0));
        cr!(BN_free(bn));
        let s = if s.len() % 2 == 1 { format!("0{}", s) } else { s };
        dict::set_item_str(&d, "serialNumber", &new_str(&s))?;
    }
    let nb = bio_to_string(|b| cr!(ASN1_TIME_print(b, cr!(X509_get0_notBefore(x)))))?;
    dict::set_item_str(&d, "notBefore", &new_str(&nb))?;
    let na = bio_to_string(|b| cr!(ASN1_TIME_print(b, cr!(X509_get0_notAfter(x)))))?;
    dict::set_item_str(&d, "notAfter", &new_str(&na))?;
    // subjectAltName
    let sans = cr!(X509_get_ext_d2i(x, NID_SUBJECT_ALT_NAME, std::ptr::null_mut(), std::ptr::null_mut()));
    if !sans.is_null() {
        let v = general_names_to_py(sans);
        cr!(GENERAL_NAMES_free(sans));
        dict::set_item_str(&d, "subjectAltName", &tuple::new_tuple(v?))?;
    }
    // authority info access
    let aia = cr!(X509_get_ext_d2i(x, NID_INFO_ACCESS, std::ptr::null_mut(), std::ptr::null_mut()));
    if !aia.is_null() {
        let mut ocsp = Vec::new();
        let mut issuers = Vec::new();
        let n = cr!(OPENSSL_sk_num(aia));
        for i in 0..n {
            #[repr(C)]
            struct AccessDescription {
                method: *mut c_void,
                location: *mut GeneralName,
            }
            let ad = cr!(OPENSSL_sk_value(aia, i)) as *const AccessDescription;
            let (m, loc) = unsafe { ((*ad).method, &*(*ad).location) };
            if loc.type_ != 6 {
                continue;
            }
            let uri = new_str(&String::from_utf8_lossy(&asn1_raw(loc.d)));
            match cr!(OBJ_obj2nid(m)) {
                NID_AD_OCSP => ocsp.push(uri),
                NID_AD_CA_ISSUERS => issuers.push(uri),
                _ => {}
            }
        }
        cr!(AUTHORITY_INFO_ACCESS_free(aia));
        if !ocsp.is_empty() {
            dict::set_item_str(&d, "OCSP", &tuple::new_tuple(ocsp))?;
        }
        if !issuers.is_empty() {
            dict::set_item_str(&d, "caIssuers", &tuple::new_tuple(issuers))?;
        }
    }
    // CRL distribution points
    let dps = cr!(X509_get_ext_d2i(x, NID_CRL_DISTRIBUTION_POINTS, std::ptr::null_mut(), std::ptr::null_mut()));
    if !dps.is_null() {
        #[repr(C)]
        struct DistPoint {
            name: *mut DistPointName,
            reasons: *mut c_void,
            issuer: *mut c_void,
            dp_reasons: c_int,
        }
        #[repr(C)]
        struct DistPointName {
            type_: c_int,
            name: *mut c_void,
        }
        let mut uris = Vec::new();
        let n = cr!(OPENSSL_sk_num(dps));
        for i in 0..n {
            let dp = cr!(OPENSSL_sk_value(dps, i)) as *const DistPoint;
            let name = unsafe { (*dp).name };
            if name.is_null() || unsafe { (*name).type_ } != 0 {
                continue;
            }
            let gns = unsafe { (*name).name };
            let m = cr!(OPENSSL_sk_num(gns));
            for j in 0..m {
                let g = cr!(OPENSSL_sk_value(gns, j)) as *const GeneralName;
                if unsafe { (*g).type_ } == 6 {
                    uris.push(new_str(&String::from_utf8_lossy(&asn1_raw(unsafe { (*g).d }))));
                }
            }
        }
        cr!(CRL_DIST_POINTS_free(dps));
        if !uris.is_empty() {
            dict::set_item_str(&d, "crlDistributionPoints", &tuple::new_tuple(uris))?;
        }
    }
    Ok(d)
}

pub fn cert_der(x: *mut c_void) -> PyResult {
    let mut out: *mut u8 = std::ptr::null_mut();
    let n = cr!(i2d_X509(x, &mut out));
    if n < 0 {
        return Err(raise_from_queue(None, "i2d_X509 failed"));
    }
    let b = bytes::new_bytes(unsafe { std::slice::from_raw_parts(out, n as usize) });
    cr!(CRYPTO_free(out as *mut c_void, cs!("_ssl"), 0));
    Ok(b)
}

fn m_test_decode_cert(_m: &PyObject, path: &PyObject) -> PyResult {
    let p = rpy_runtime::modules::posix::path::fspath(path)?;
    let pb = if is_bytes(&p) { bytes::bytes_data(&p).to_vec() } else { str_of(&p).into_bytes() };
    let c = CString::new(pb).map_err(|_| err!(ValueError, "embedded null byte"))?;
    let b = cr!(BIO_new_file(c.as_ptr(), cs!("r")));
    if b.is_null() {
        cr!(ERR_clear_error());
        return Err(raise_simple(&st().ssl_error, 0, "Can't open file"));
    }
    let x = cr!(PEM_read_bio_X509(b, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()));
    cr!(BIO_free(b));
    if x.is_null() {
        return Err(raise_from_queue(None, "Error decoding PEM-encoded file"));
    }
    let r = decode_certificate(x);
    cr!(X509_free(x));
    r
}

// ------------------------------------------------------------------ Certificate

pub struct Certificate {
    pub x: *mut c_void,
}
impl Trav for Certificate {}
impl Drop for Certificate {
    fn drop(&mut self) {
        if !self.x.is_null() {
            cr!(X509_free(self.x));
        }
    }
}
unsafe impl Send for Certificate {}

pub static mut CERTIFICATE_TYPE: PyTypeObject = PyTypeObject::ZERO;

/// Wrap a certificate (takes a new reference to `x`).
pub fn new_certificate(x: *mut c_void) -> PyResult {
    cr!(X509_up_ref(x));
    fc::robj_new(&raw mut CERTIFICATE_TYPE, Certificate { x })
}

fn certobj(o: &PyObject) -> &'static mut Certificate {
    unsafe { &mut *(fc::rs::<Certificate>(o) as *mut Certificate) }
}

fn cert_public_bytes(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let fmt = if args.is_empty() { 1 } else { rpy_runtime::abstract_::as_i64(args[0])? };
    let x = certobj(o).x;
    match fmt {
        2 => cert_der(x),
        1 => {
            let b = cr!(BIO_new(cr!(BIO_s_mem())));
            cr!(PEM_write_bio_X509(b, x));
            let mut out = Vec::new();
            let mut buf = vec![0u8; 4096];
            loop {
                let n = cr!(BIO_read(b, buf.as_mut_ptr() as *mut c_void, 4096));
                if n <= 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n as usize]);
            }
            cr!(BIO_free(b));
            Ok(new_str(&String::from_utf8_lossy(&out)))
        }
        _ => raise!(ValueError, "Unsupported format"),
    }
}

fn cert_get_info(o: &PyObject) -> PyResult {
    decode_certificate(certobj(o).x)
}

unsafe extern "C" fn cert_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let x = certobj(&*o).x;
        let mut buf = [0 as c_char; 512];
        cr!(X509_NAME_oneline(cr!(X509_get_subject_name(x)), buf.as_mut_ptr(), 512));
        new_str(&format!("<Certificate '{}'>", cstr(buf.as_ptr()))).into_raw()
    }
}

unsafe extern "C" fn cert_hash(o: *mut PyObject) -> isize {
    unsafe {
        match cert_der(certobj(&*o).x) {
            Ok(b) => rpy_runtime::types::bytes::bytes_hash(&b),
            Err(_) => -1,
        }
    }
}

unsafe extern "C" fn cert_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        if (*b).type_ptr() != &raw mut CERTIFICATE_TYPE || (op != 2 && op != 3) {
            return rpy_runtime::types::none::not_implemented().to_ref().into_raw();
        }
        let (da, db) = match (cert_der(certobj(&*a).x), cert_der(certobj(&*b).x)) {
            (Ok(x), Ok(y)) => (x, y),
            _ => return std::ptr::null_mut(),
        };
        let eq = bytes::bytes_data(&da) == bytes::bytes_data(&db);
        rpy_runtime::types::bool_::new_bool(if op == 2 { eq } else { !eq }).into_raw()
    }
}

// ------------------------------------------------------------------ MemoryBIO

pub struct MemBio {
    pub bio: *mut c_void,
    pub eof_written: bool,
}
impl Trav for MemBio {}
impl Drop for MemBio {
    fn drop(&mut self) {
        if !self.bio.is_null() {
            cr!(BIO_free(self.bio));
        }
    }
}

pub static mut MEMBIO_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn membio(o: &PyObject) -> &'static mut MemBio {
    unsafe { &mut *(fc::rs::<MemBio>(o) as *mut MemBio) }
}

pub fn is_membio(o: &PyObject) -> bool {
    o.type_ptr() == &raw mut MEMBIO_TYPE
}

unsafe extern "C" fn membio_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if !fc::targs(args).is_empty() || fc::kw_len(kw) > 0 {
                raise!(TypeError, "MemoryBIO() takes no arguments");
            }
            let bio = cr!(BIO_new(cr!(BIO_s_mem())));
            if bio.is_null() {
                raise!(MemoryError, "failed to allocate BIO");
            }
            // a read from an empty BIO means "want read", not EOF
            cr!(BIO_ctrl(bio, BIO_C_SET_BUF_MEM_EOF_RETURN, -1, std::ptr::null_mut()));
            fc::robj_new(tp, MemBio { bio, eof_written: false })
        })();
        into_c(r)
    }
}

fn membio_pending(o: &PyObject) -> PyResult {
    Ok(new_int(cr!(BIO_ctrl_pending(membio(o).bio)) as i64))
}

fn membio_eof(o: &PyObject) -> PyResult {
    let m = membio(o);
    let pending = cr!(BIO_ctrl_pending(m.bio));
    Ok(rpy_runtime::types::bool_::new_bool(pending == 0 && m.eof_written))
}

fn membio_read(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let m = membio(o);
    let avail = cr!(BIO_ctrl_pending(m.bio)) as i64;
    let mut n = if args.is_empty() { -1 } else { rpy_runtime::abstract_::as_i64(args[0])? };
    if n < 0 || n > avail {
        n = avail;
    }
    if n == 0 {
        return Ok(bytes::new_bytes(&[]));
    }
    let mut buf = vec![0u8; n as usize];
    let r = cr!(BIO_read(m.bio, buf.as_mut_ptr() as *mut c_void, n as c_int));
    if r < 0 {
        return Err(raise_from_queue(None, "BIO_read failed"));
    }
    buf.truncate(r as usize);
    Ok(bytes::new_bytes(&buf))
}

fn membio_write(o: &PyObject, data: &PyObject) -> PyResult {
    let m = membio(o);
    let mut view: Py_buffer = unsafe { std::mem::zeroed() };
    unsafe {
        if capi::PyObject_GetBuffer(data.as_ptr(), &mut view, 0) < 0 {
            return Err(PyErr);
        }
    }
    let r = (|| -> PyResult {
        if view.len > i32::MAX as isize {
            raise!(OverflowError, "string longer than {} bytes", i32::MAX);
        }
        if m.eof_written {
            return Err(raise_simple(&st().ssl_error, 0, "cannot write() after write_eof()"));
        }
        let n = cr!(BIO_write(m.bio, view.buf, view.len as c_int));
        if n < 0 {
            return Err(raise_from_queue(None, "BIO_write failed"));
        }
        Ok(new_int(n as i64))
    })();
    unsafe { capi::PyBuffer_Release(&mut view) };
    r
}

fn membio_write_eof(o: &PyObject) -> PyResult {
    let m = membio(o);
    m.eof_written = true;
    // reads from an empty BIO now return 0 (EOF)
    cr!(BIO_clear_flags(m.bio, BIO_FLAGS_SHOULD_RETRY));
    cr!(BIO_ctrl(m.bio, BIO_C_SET_BUF_MEM_EOF_RETURN, 0, std::ptr::null_mut()));
    Ok(none())
}

// ------------------------------------------------------------------ SSLSession

pub struct Session {
    pub sess: *mut c_void,
    pub ctx: PyRef,
}
impl Trav for Session {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        v(self.ctx.as_ptr())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.sess.is_null() {
            sl!(SSL_SESSION_free(self.sess));
        }
    }
}

pub static mut SESSION_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn session(o: &PyObject) -> &'static mut Session {
    unsafe { &mut *(fc::rs::<Session>(o) as *mut Session) }
}

pub fn is_session(o: &PyObject) -> bool {
    o.type_ptr() == &raw mut SESSION_TYPE
}

pub fn new_session(sess: *mut c_void, ctx: PyRef) -> PyResult {
    fc::robj_new(&raw mut SESSION_TYPE, Session { sess, ctx })
}

fn session_id_bytes(o: &PyObject) -> Vec<u8> {
    let mut n: u32 = 0;
    let p = sl!(SSL_SESSION_get_id(session(o).sess, &mut n));
    if p.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(p, n as usize) }.to_vec() }
}

fn sess_id(o: &PyObject) -> PyResult {
    Ok(bytes::new_bytes(&session_id_bytes(o)))
}
fn sess_time(o: &PyObject) -> PyResult {
    Ok(new_int(sl!(SSL_SESSION_get_time(session(o).sess)) as i64))
}
fn sess_timeout(o: &PyObject) -> PyResult {
    Ok(new_int(sl!(SSL_SESSION_get_timeout(session(o).sess)) as i64))
}
fn sess_hint(o: &PyObject) -> PyResult {
    Ok(new_int(sl!(SSL_SESSION_get_ticket_lifetime_hint(session(o).sess)) as i64))
}
fn sess_has_ticket(o: &PyObject) -> PyResult {
    Ok(rpy_runtime::types::bool_::new_bool(sl!(SSL_SESSION_has_ticket(session(o).sess)) != 0))
}

unsafe extern "C" fn sess_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        if !is_session(&*b) || (op != 2 && op != 3) {
            return rpy_runtime::types::none::not_implemented().to_ref().into_raw();
        }
        let eq = a == b || session_id_bytes(&*a) == session_id_bytes(&*b);
        rpy_runtime::types::bool_::new_bool(if op == 2 { eq } else { !eq }).into_raw()
    }
}

// ------------------------------------------------------------------ module functions

fn m_rand_bytes(_m: &PyObject, n: &PyObject) -> PyResult {
    let n = rpy_runtime::abstract_::as_i64(n)?;
    if n < 0 {
        raise!(ValueError, "num must be positive");
    }
    let mut buf = vec![0u8; n as usize];
    if cr!(RAND_bytes(buf.as_mut_ptr(), n as c_int)) != 1 {
        return Err(raise_from_queue(None, "RAND_bytes failed"));
    }
    Ok(bytes::new_bytes(&buf))
}

fn m_rand_status(_m: &PyObject) -> PyResult {
    Ok(rpy_runtime::types::bool_::new_bool(cr!(RAND_status()) == 1))
}

fn m_rand_add(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 2 {
        raise!(TypeError, "RAND_add expected 2 arguments, got {}", args.len());
    }
    let data: Vec<u8> = if is_str(args[0]) {
        str_of(args[0]).into_bytes()
    } else {
        let mut view: Py_buffer = unsafe { std::mem::zeroed() };
        unsafe {
            if capi::PyObject_GetBuffer(args[0].as_ptr(), &mut view, 0) < 0 {
                return Err(PyErr);
            }
            let v = std::slice::from_raw_parts(view.buf as *const u8, view.len as usize).to_vec();
            capi::PyBuffer_Release(&mut view);
            v
        }
    };
    let entropy = rpy_runtime::abstract_::float_value(args[1])?;
    cr!(RAND_add(data.as_ptr() as *const c_void, data.len() as c_int, entropy));
    Ok(none())
}

fn asn1obj_tuple(obj: *mut c_void) -> PyResult {
    let nid = cr!(OBJ_obj2nid(obj));
    let (sn, ln) = if nid != 0 { (new_str(&cstr(cr!(OBJ_nid2sn(nid)))), new_str(&cstr(cr!(OBJ_nid2ln(nid))))) } else { (none(), none()) };
    let mut buf = [0 as c_char; 256];
    cr!(OBJ_obj2txt(buf.as_mut_ptr(), 256, obj, 1));
    Ok(tuple::new_tuple(vec![new_int(nid as i64), sn, ln, new_str(&cstr(buf.as_ptr()))]))
}

fn m_txt2obj(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("txt2obj", &["txt", "name"], 0, 1);
    let [txt, name] = rpy_runtime::args::parse::<2>(&P, args, kw)?;
    let txt = txt.unwrap();
    if !is_str(txt) {
        raise!(TypeError, "txt2obj() argument 'txt' must be str, not {}", tname(txt));
    }
    let byname = match name {
        Some(n) => rpy_runtime::abstract_::is_true(n)?,
        None => false,
    };
    let c = cstring(&str_of(txt));
    let obj = cr!(OBJ_txt2obj(c.as_ptr(), if byname { 0 } else { 1 }));
    if obj.is_null() {
        cr!(ERR_clear_error());
        raise!(ValueError, "unknown object '{}'", str_of(txt));
    }
    let r = asn1obj_tuple(obj);
    cr!(ASN1_OBJECT_free(obj));
    r
}

fn m_nid2obj(_m: &PyObject, nid: &PyObject) -> PyResult {
    let n = rpy_runtime::abstract_::as_i64(nid)?;
    if n < 1 {
        raise!(ValueError, "NID must be positive.");
    }
    let obj = cr!(OBJ_nid2obj(n as c_int));
    if obj.is_null() {
        cr!(ERR_clear_error());
        raise!(ValueError, "unknown NID {}", n);
    }
    let r = asn1obj_tuple(obj);
    cr!(ASN1_OBJECT_free(obj));
    r
}

fn m_get_default_verify_paths(_m: &PyObject) -> PyResult {
    Ok(tuple::new_tuple(vec![
        new_str(&cstr(cr!(X509_get_default_cert_file_env()))),
        new_str(&cstr(cr!(X509_get_default_cert_file()))),
        new_str(&cstr(cr!(X509_get_default_cert_dir_env()))),
        new_str(&cstr(cr!(X509_get_default_cert_dir()))),
    ]))
}

// ------------------------------------------------------------------ init

pub fn init() -> PyResult {
    if let Err(e) = ffi::LibCrypto::get() {
        raise!(ImportError, "libcrypto could not be loaded: {}", e);
    }
    if let Err(e) = ffi::LibSsl::get() {
        raise!(ImportError, "libssl could not be loaded: {}", e);
    }
    let methods = rpy_runtime::mdefs![
        ("RAND_bytes", O, m_rand_bytes),
        ("RAND_status", NOARGS, m_rand_status),
        ("RAND_add", FAST, m_rand_add),
        ("txt2obj", FASTKW, m_txt2obj),
        ("nid2obj", O, m_nid2obj),
        ("_test_decode_cert", O, m_test_decode_cert),
        ("get_default_verify_paths", NOARGS, m_get_default_verify_paths),
    ];
    let (m, d) = fc::new_module("_ssl", methods, None)?;
    // exceptions
    let oserror = unsafe { PyRef::from_borrowed(rpy_runtime::types::exceptions::OSError() as *mut PyObject) };
    let valueerror = unsafe { PyRef::from_borrowed(rpy_runtime::types::exceptions::ValueError() as *mut PyObject) };
    let ssl_error = new_exc("ssl.SSLError", oserror, None)?;
    let one = |b: &PyRef| tuple::new_tuple(vec![b.clone()]);
    let cert_verify_error = new_exc(
        "ssl.SSLCertVerificationError",
        tuple::new_tuple(vec![ssl_error.clone(), valueerror]),
        None,
    )?;
    let zero_return_error = new_exc("ssl.SSLZeroReturnError", one(&ssl_error), None)?;
    let want_read_error = new_exc("ssl.SSLWantReadError", one(&ssl_error), None)?;
    let want_write_error = new_exc("ssl.SSLWantWriteError", one(&ssl_error), None)?;
    let syscall_error = new_exc("ssl.SSLSyscallError", one(&ssl_error), None)?;
    let eof_error = new_exc("ssl.SSLEOFError", one(&ssl_error), None)?;
    for (n, v) in [
        ("SSLError", &ssl_error),
        ("SSLCertVerificationError", &cert_verify_error),
        ("SSLZeroReturnError", &zero_return_error),
        ("SSLWantReadError", &want_read_error),
        ("SSLWantWriteError", &want_write_error),
        ("SSLSyscallError", &syscall_error),
        ("SSLEOFError", &eof_error),
    ] {
        dict::set_item_str(&d, n, v)?;
    }
    let _ = STATE.set(State { ssl_error, cert_verify_error, zero_return_error, want_read_error, want_write_error, syscall_error, eof_error });

    unsafe {
        let t = &raw mut CERTIFICATE_TYPE;
        fc::init_robj_type::<Certificate>(t, c"_ssl.Certificate", Py_TPFLAGS_DISALLOW_INSTANTIATION);
        (*t).tp_repr = Some(cert_repr);
        (*t).tp_hash = Some(cert_hash);
        (*t).tp_richcompare = Some(cert_richcompare);
        (*t).tp_methods = rpy_runtime::mdefs![
            ("public_bytes", FAST, cert_public_bytes),
            ("get_info", NOARGS, cert_get_info),
        ];
        rpy_runtime::types::typeobject::type_ready(t)?;
        dict::set_item_str(&d, "Certificate", &rpy_runtime::types::typeobject::type_ref(t))?;

        let t = &raw mut MEMBIO_TYPE;
        fc::init_robj_type::<MemBio>(t, c"_ssl.MemoryBIO", 0);
        (*t).tp_new = Some(membio_new);
        (*t).tp_methods = rpy_runtime::mdefs![
            ("read", FAST, membio_read),
            ("write", O, membio_write),
            ("write_eof", NOARGS, membio_write_eof),
        ];
        (*t).tp_getset = rpy_runtime::getset_table![
            ("pending", membio_pending),
            ("eof", membio_eof),
        ];
        rpy_runtime::types::typeobject::type_ready(t)?;
        dict::set_item_str(&d, "MemoryBIO", &rpy_runtime::types::typeobject::type_ref(t))?;

        let t = &raw mut SESSION_TYPE;
        fc::init_robj_type::<Session>(t, c"_ssl.SSLSession", Py_TPFLAGS_DISALLOW_INSTANTIATION);
        (*t).tp_richcompare = Some(sess_richcompare);
        (*t).tp_getset = rpy_runtime::getset_table![
            ("id", sess_id),
            ("time", sess_time),
            ("timeout", sess_timeout),
            ("ticket_lifetime_hint", sess_hint),
            ("has_ticket", sess_has_ticket),
        ];
        rpy_runtime::types::typeobject::type_ready(t)?;
        dict::set_item_str(&d, "SSLSession", &rpy_runtime::types::typeobject::type_ref(t))?;
    }
    context::init_type()?;
    socket::init_type()?;
    dict::set_item_str(&d, "_SSLContext", &rpy_runtime::types::typeobject::type_ref(context::context_type()))?;
    dict::set_item_str(&d, "_SSLSocket", &rpy_runtime::types::typeobject::type_ref(socket::socket_type()))?;

    // constants
    for &(n, v) in CONSTANTS {
        dict::set_item_str(&d, n, &new_int(v))?;
    }
    for (n, v) in [
        ("HAS_SNI", true),
        ("HAS_TLS_UNIQUE", true),
        ("HAS_ECDH", true),
        ("HAS_NPN", false),
        ("HAS_ALPN", true),
        ("HAS_SSLv2", false),
        ("HAS_SSLv3", false),
        ("HAS_TLSv1", true),
        ("HAS_TLSv1_1", true),
        ("HAS_TLSv1_2", true),
        ("HAS_TLSv1_3", true),
        ("HAS_NEVER_CHECK_COMMON_NAME", true),
        ("HAS_PSK", false),
        ("HAS_PSK_TLS13", false),
        ("HAS_PHA", true),
    ] {
        dict::set_item_str(&d, n, &rpy_runtime::types::bool_::new_bool(v))?;
    }
    let vnum = cr!(OpenSSL_version_num()) as u64;
    dict::set_item_str(&d, "OPENSSL_VERSION_NUMBER", &rpy_runtime::types::int::new_int_u128(vnum as u128))?;
    dict::set_item_str(&d, "OPENSSL_VERSION", &new_str(&cstr(cr!(OpenSSL_version(0)))))?;
    let info = version_info(vnum);
    dict::set_item_str(&d, "OPENSSL_VERSION_INFO", &info)?;
    dict::set_item_str(&d, "_OPENSSL_API_VERSION", &info)?;
    dict::set_item_str(&d, "_DEFAULT_CIPHERS", &new_str("@SECLEVEL=2:ECDH+AESGCM:ECDH+CHACHA20:ECDH+AES:DHE+AES:!aNULL:!eNULL:!aDSS:!SHA1:!AESCCM"))?;
    let errs = dict::new_dict();
    dict::set_item_str(&d, "err_codes_to_names", &errs)?;
    let libs = dict::new_dict();
    for l in 1..=128 {
        if let Some(n) = lib_name(l) {
            dict::set_item(&libs, &new_int(l as i64), &new_str(n))?;
        }
    }
    dict::set_item_str(&d, "lib_codes_to_names", &libs)?;
    let _ = (getattr_opt, call, c_long::default());
    Ok(m)
}

fn version_info(v: u64) -> PyRef {
    // OpenSSL 3: MNNFFPPS (major, minor, fix, patch, status)
    let major = (v >> 28) & 0xf;
    let minor = (v >> 20) & 0xff;
    let fix = (v >> 12) & 0xff;
    let patch = (v >> 4) & 0xff;
    let status = v & 0xf;
    tuple::new_tuple(vec![new_int(major as i64), new_int(minor as i64), new_int(fix as i64), new_int(patch as i64), new_int(status as i64)])
}

pub const PROTOCOL_SSLV23: i64 = 2;
pub const PROTOCOL_TLS_CLIENT: i64 = 0x10;
pub const PROTOCOL_TLS_SERVER: i64 = 0x11;

static CONSTANTS: &[(&str, i64)] = &[
    ("PROTOCOL_SSLv23", 2),
    ("PROTOCOL_TLS", 2),
    ("PROTOCOL_TLS_CLIENT", 0x10),
    ("PROTOCOL_TLS_SERVER", 0x11),
    ("PROTOCOL_TLSv1", 3),
    ("PROTOCOL_TLSv1_1", 4),
    ("PROTOCOL_TLSv1_2", 5),
    ("PROTO_MINIMUM_SUPPORTED", -2),
    ("PROTO_MAXIMUM_SUPPORTED", -1),
    ("PROTO_SSLv3", 0x300),
    ("PROTO_TLSv1", 0x301),
    ("PROTO_TLSv1_1", 0x302),
    ("PROTO_TLSv1_2", 0x303),
    ("PROTO_TLSv1_3", 0x304),
    ("CERT_NONE", 0),
    ("CERT_OPTIONAL", 1),
    ("CERT_REQUIRED", 2),
    ("VERIFY_DEFAULT", 0),
    ("VERIFY_CRL_CHECK_LEAF", 0x4),
    ("VERIFY_CRL_CHECK_CHAIN", 0x4 | 0x8),
    ("VERIFY_X509_STRICT", 0x20),
    ("VERIFY_ALLOW_PROXY_CERTS", 0x40),
    ("VERIFY_X509_TRUSTED_FIRST", 0x8000),
    ("VERIFY_X509_PARTIAL_CHAIN", 0x80000),
    ("OP_ALL", 0x80000850),
    ("OP_NO_SSLv2", 0),
    ("OP_NO_SSLv3", 1 << 25),
    ("OP_NO_TLSv1", 1 << 26),
    ("OP_NO_TLSv1_1", 1 << 28),
    ("OP_NO_TLSv1_2", 1 << 27),
    ("OP_NO_TLSv1_3", 1 << 29),
    ("OP_CIPHER_SERVER_PREFERENCE", 1 << 22),
    ("OP_SINGLE_DH_USE", 0),
    ("OP_SINGLE_ECDH_USE", 0),
    ("OP_NO_COMPRESSION", 1 << 17),
    ("OP_NO_TICKET", 1 << 14),
    ("OP_NO_RENEGOTIATION", 1 << 30),
    ("OP_ENABLE_MIDDLEBOX_COMPAT", 1 << 20),
    ("OP_IGNORE_UNEXPECTED_EOF", 1 << 7),
    ("OP_LEGACY_SERVER_CONNECT", 1 << 2),
    ("OP_ENABLE_KTLS", 1 << 3),
    ("HOSTFLAG_ALWAYS_CHECK_SUBJECT", 0x1),
    ("HOSTFLAG_NEVER_CHECK_SUBJECT", 0x20),
    ("HOSTFLAG_NO_WILDCARDS", 0x2),
    ("HOSTFLAG_NO_PARTIAL_WILDCARDS", 0x4),
    ("HOSTFLAG_MULTI_LABEL_WILDCARDS", 0x8),
    ("HOSTFLAG_SINGLE_LABEL_SUBDOMAINS", 0x10),
    ("SSL_ERROR_ZERO_RETURN", 6),
    ("SSL_ERROR_WANT_READ", 2),
    ("SSL_ERROR_WANT_WRITE", 3),
    ("SSL_ERROR_WANT_X509_LOOKUP", 4),
    ("SSL_ERROR_SYSCALL", 5),
    ("SSL_ERROR_SSL", 1),
    ("SSL_ERROR_WANT_CONNECT", 7),
    ("SSL_ERROR_EOF", 8),
    ("SSL_ERROR_INVALID_ERROR_CODE", 10),
    ("ENCODING_PEM", 1),
    ("ENCODING_DER", 2),
    ("ALERT_DESCRIPTION_CLOSE_NOTIFY", 0),
    ("ALERT_DESCRIPTION_UNEXPECTED_MESSAGE", 10),
    ("ALERT_DESCRIPTION_BAD_RECORD_MAC", 20),
    ("ALERT_DESCRIPTION_RECORD_OVERFLOW", 22),
    ("ALERT_DESCRIPTION_DECOMPRESSION_FAILURE", 30),
    ("ALERT_DESCRIPTION_HANDSHAKE_FAILURE", 40),
    ("ALERT_DESCRIPTION_BAD_CERTIFICATE", 42),
    ("ALERT_DESCRIPTION_UNSUPPORTED_CERTIFICATE", 43),
    ("ALERT_DESCRIPTION_CERTIFICATE_REVOKED", 44),
    ("ALERT_DESCRIPTION_CERTIFICATE_EXPIRED", 45),
    ("ALERT_DESCRIPTION_CERTIFICATE_UNKNOWN", 46),
    ("ALERT_DESCRIPTION_ILLEGAL_PARAMETER", 47),
    ("ALERT_DESCRIPTION_UNKNOWN_CA", 48),
    ("ALERT_DESCRIPTION_ACCESS_DENIED", 49),
    ("ALERT_DESCRIPTION_DECODE_ERROR", 50),
    ("ALERT_DESCRIPTION_DECRYPT_ERROR", 51),
    ("ALERT_DESCRIPTION_PROTOCOL_VERSION", 70),
    ("ALERT_DESCRIPTION_INSUFFICIENT_SECURITY", 71),
    ("ALERT_DESCRIPTION_INTERNAL_ERROR", 80),
    ("ALERT_DESCRIPTION_USER_CANCELLED", 90),
    ("ALERT_DESCRIPTION_NO_RENEGOTIATION", 100),
    ("ALERT_DESCRIPTION_UNSUPPORTED_EXTENSION", 110),
    ("ALERT_DESCRIPTION_CERTIFICATE_UNOBTAINABLE", 111),
    ("ALERT_DESCRIPTION_UNRECOGNIZED_NAME", 112),
    ("ALERT_DESCRIPTION_BAD_CERTIFICATE_STATUS_RESPONSE", 113),
    ("ALERT_DESCRIPTION_BAD_CERTIFICATE_HASH_VALUE", 114),
    ("ALERT_DESCRIPTION_UNKNOWN_PSK_IDENTITY", 115),
];

pub fn _unused(_: *const c_char) {}
