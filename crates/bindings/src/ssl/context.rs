//! _ssl._SSLContext

use super::*;
use std::ffi::c_uint;

pub struct Ctx {
    pub ctx: *mut c_void,
    pub protocol: i64,
    pub check_hostname: bool,
    pub post_handshake_auth: bool,
    pub sni_callback: Option<PyRef>,
    pub msg_callback: Option<PyRef>,
    pub keylog_filename: Option<PyRef>,
    pub alpn: Vec<u8>,
}

impl Trav for Ctx {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        for o in [&self.sni_callback, &self.msg_callback, &self.keylog_filename] {
            if let Some(r) = o {
                let x = v(r.as_ptr());
                if x != 0 {
                    return x;
                }
            }
        }
        0
    }
}

impl Drop for Ctx {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            sl!(SSL_CTX_free(self.ctx));
        }
    }
}

pub static mut CONTEXT_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn context_type() -> *mut PyTypeObject {
    &raw mut CONTEXT_TYPE
}

pub fn ctx(o: &PyObject) -> &'static mut Ctx {
    unsafe { &mut *(fc::rs::<Ctx>(o) as *mut Ctx) }
}

pub fn is_context(o: &PyObject) -> bool {
    crate::ctypes::subtype(o.type_ptr(), context_type())
}

/// ex_data slot of SSL_CTX objects pointing back at the Python context.
pub const CTX_EX_INDEX: c_int = 0;

fn deprecated(msg: &str) -> PyResult<()> {
    let c = cstring(msg);
    if unsafe { capi::PyErr_WarnEx(capi::exc::DeprecationWarning(), c.as_ptr(), 2) } < 0 {
        return Err(PyErr);
    }
    Ok(())
}

unsafe extern "C" fn ctx_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = fc::VcArgs::from_c(args, kw);
        into_c(ctx_new_impl(tp, &a))
    }
}

fn ctx_new_impl(tp: *mut PyTypeObject, a: &fc::VcArgs) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("_SSLContext", &["protocol"], 1, 1);
    let [protocol] = a.parse::<1>(&P)?;
    let protocol = rpy_runtime::abstract_::as_i64(&protocol.unwrap())?;
    let method = match protocol {
        2 => {
            deprecated("ssl.PROTOCOL_TLS is deprecated")?;
            sl!(TLS_method())
        }
        0x10 => sl!(TLS_client_method()),
        0x11 => sl!(TLS_server_method()),
        3 => {
            deprecated("ssl.PROTOCOL_TLSv1 is deprecated")?;
            sl!(TLSv1_method())
        }
        4 => {
            deprecated("ssl.PROTOCOL_TLSv1_1 is deprecated")?;
            sl!(TLSv1_1_method())
        }
        5 => {
            deprecated("ssl.PROTOCOL_TLSv1_2 is deprecated")?;
            sl!(TLSv1_2_method())
        }
        _ => raise!(ValueError, "invalid or unsupported protocol version {}", protocol),
    };
    let c = sl!(SSL_CTX_new(method));
    if c.is_null() {
        return Err(raise_from_queue(None, "failed to allocate SSL context"));
    }
    let is_client = protocol == 0x10;
    let obj = fc::robj_new(
        tp,
        Ctx {
            ctx: c,
            protocol,
            check_hostname: is_client,
            post_handshake_auth: false,
            sni_callback: None,
            msg_callback: None,
            keylog_filename: None,
            alpn: Vec::new(),
        },
    )?;
    sl!(SSL_CTX_set_ex_data(c, CTX_EX_INDEX, obj.as_ptr() as *mut c_void));
    // defaults
    let opts: u64 = 0x8000_0850u64 | (1 << 25) | (1 << 17) | (1 << 22) | (1 << 20);
    sl!(SSL_CTX_set_options(c, opts));
    set_verify_mode_raw(&obj, if is_client { 2 } else { 0 })?;
    let ciphers = CString::new("@SECLEVEL=2:ECDH+AESGCM:ECDH+CHACHA20:ECDH+AES:DHE+AES:!aNULL:!eNULL:!aDSS:!SHA1:!AESCCM").unwrap();
    if sl!(SSL_CTX_set_cipher_list(c, ciphers.as_ptr())) == 0 {
        cr!(ERR_clear_error());
    }
    sl!(SSL_CTX_ctrl(c, SSL_CTRL_MODE, SSL_MODE_ACCEPT_MOVING_WRITE_BUFFER | SSL_MODE_AUTO_RETRY, std::ptr::null_mut()));
    if matches!(protocol, 2 | 0x10 | 0x11) {
        sl!(SSL_CTX_ctrl(c, SSL_CTRL_SET_MIN_PROTO_VERSION, 0x303, std::ptr::null_mut()));
    }
    let sid = b"Python";
    sl!(SSL_CTX_set_session_id_context(c, sid.as_ptr(), sid.len() as c_uint));
    let store = sl!(SSL_CTX_get_cert_store(c));
    let param = cr!(X509_STORE_get0_param(store));
    cr!(X509_VERIFY_PARAM_set_flags(param, 0x8000)); // trusted first
    cr!(X509_VERIFY_PARAM_set_hostflags(param, 0x4)); // no partial wildcards
    // SNI callback dispatcher is always installed; it does nothing without a Python callback
    sl!(SSL_CTX_callback_ctrl(c, SSL_CTRL_SET_TLSEXT_SERVERNAME_CB, servername_cb as *mut c_void));
    Ok(obj)
}

// ------------------------------------------------------------------ verify mode / options

fn set_verify_mode_raw(o: &PyObject, n: i64) -> PyResult<()> {
    let c = ctx(o);
    let mut mode = match n {
        0 => SSL_VERIFY_NONE,
        1 => SSL_VERIFY_PEER,
        2 => SSL_VERIFY_PEER | SSL_VERIFY_FAIL_IF_NO_PEER_CERT,
        _ => raise!(ValueError, "invalid value for verify_mode"),
    };
    if c.post_handshake_auth && n != 0 && c.protocol != 0x10 {
        mode |= SSL_VERIFY_POST_HANDSHAKE;
    }
    sl!(SSL_CTX_set_verify(c.ctx, mode, std::ptr::null_mut()));
    Ok(())
}

fn get_verify_mode(o: &PyObject) -> PyResult {
    let m = sl!(SSL_CTX_get_verify_mode(ctx(o).ctx)) & (SSL_VERIFY_PEER | SSL_VERIFY_FAIL_IF_NO_PEER_CERT);
    Ok(new_int(match m {
        0 => 0,
        1 => 1,
        _ => 2,
    }))
}

fn set_verify_mode(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(AttributeError, "cannot delete attribute"),
    };
    let n = rpy_runtime::abstract_::as_i64(v)?;
    if n == 0 && ctx(o).check_hostname {
        raise!(ValueError, "Cannot set verify_mode to CERT_NONE when check_hostname is enabled.");
    }
    set_verify_mode_raw(o, n)
}

fn get_check_hostname(o: &PyObject) -> PyResult {
    Ok(rpy_runtime::types::bool_::new_bool(ctx(o).check_hostname))
}

fn set_check_hostname(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(AttributeError, "cannot delete attribute"),
    };
    let b = rpy_runtime::abstract_::is_true(v)?;
    if b && sl!(SSL_CTX_get_verify_mode(ctx(o).ctx)) == SSL_VERIFY_NONE {
        set_verify_mode_raw(o, 2)?;
    }
    ctx(o).check_hostname = b;
    Ok(())
}

fn get_options(o: &PyObject) -> PyResult {
    Ok(rpy_runtime::types::int::new_int_u128(sl!(SSL_CTX_get_options(ctx(o).ctx)) as u128))
}

fn set_options(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(AttributeError, "cannot delete attribute"),
    };
    if !rpy_runtime::types::int::is_int(v) {
        raise!(TypeError, "options must be an int, not {}", tname(v));
    }
    let neg = unsafe { capi::PyObject_RichCompareBool(v.as_ptr(), new_int(0).as_ptr(), 0) };
    if neg < 0 {
        return Err(PyErr);
    }
    if neg == 1 {
        raise!(ValueError, "options must be a non-negative integer");
    }
    let new = unsafe { capi::PyLong_AsUnsignedLongLong(v.as_ptr()) };
    if new == u64::MAX && err_occurred() {
        return Err(PyErr);
    }
    let c = ctx(o).ctx;
    let cur = sl!(SSL_CTX_get_options(c));
    let clear = cur & !new;
    let set = !cur & new;
    let no_proto = (1u64 << 25) | (1 << 26) | (1 << 27) | (1 << 28) | (1 << 29);
    if set & no_proto != 0 {
        deprecated("ssl.OP_NO_SSL*/ssl.OP_NO_TLS* options are deprecated")?;
    }
    if clear != 0 {
        sl!(SSL_CTX_clear_options(c, clear));
    }
    if set != 0 {
        sl!(SSL_CTX_set_options(c, set));
    }
    Ok(())
}

fn store_param(o: &PyObject) -> *mut c_void {
    let store = sl!(SSL_CTX_get_cert_store(ctx(o).ctx));
    cr!(X509_STORE_get0_param(store))
}

fn get_verify_flags(o: &PyObject) -> PyResult {
    Ok(new_int(cr!(X509_VERIFY_PARAM_get_flags(store_param(o))) as i64))
}

fn set_verify_flags(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(AttributeError, "cannot delete attribute"),
    };
    let new = crate::ctypes::int_mask(v)? as std::ffi::c_ulong;
    let p = store_param(o);
    let cur = cr!(X509_VERIFY_PARAM_get_flags(p));
    let clear = cur & !new;
    let set = !cur & new;
    if clear != 0 && cr!(X509_VERIFY_PARAM_clear_flags(p, clear)) == 0 {
        return Err(raise_from_queue(None, "failed to clear verify flags"));
    }
    if set != 0 && cr!(X509_VERIFY_PARAM_set_flags(p, set)) == 0 {
        return Err(raise_from_queue(None, "failed to set verify flags"));
    }
    Ok(())
}

fn get_host_flags(o: &PyObject) -> PyResult {
    Ok(new_int(cr!(X509_VERIFY_PARAM_get_hostflags(store_param(o))) as i64))
}

fn set_host_flags(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(AttributeError, "cannot delete attribute"),
    };
    let f = crate::ctypes::int_mask(v)? as c_uint;
    cr!(X509_VERIFY_PARAM_set_hostflags(store_param(o), f));
    Ok(())
}

fn get_protocol(o: &PyObject) -> PyResult {
    Ok(new_int(ctx(o).protocol))
}

fn get_proto_version(o: &PyObject, cmd: c_int, default: i64) -> PyResult {
    let v = sl!(SSL_CTX_ctrl(ctx(o).ctx, cmd, 0, std::ptr::null_mut()));
    Ok(new_int(if v == 0 { default } else { v as i64 }))
}

fn set_proto_version(o: &PyObject, v: Option<&PyObject>, max: bool) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(AttributeError, "cannot delete attribute"),
    };
    let n = rpy_runtime::abstract_::as_i64(v)?;
    let c = ctx(o);
    if !matches!(c.protocol, 2 | 0x10 | 0x11) {
        raise!(ValueError, "The context's protocol doesn't support modification of highest and lowest version.");
    }
    let val = match n {
        -2 if max => 0x300,
        -1 if !max => 0x304,
        -2 | -1 => 0,
        0x300 | 0x301 | 0x302 | 0x303 | 0x304 => n,
        _ => raise!(ValueError, "Unsupported TLS/SSL version 0x{:x}", n),
    };
    let cmd = if max { SSL_CTRL_SET_MAX_PROTO_VERSION } else { SSL_CTRL_SET_MIN_PROTO_VERSION };
    if sl!(SSL_CTX_ctrl(c.ctx, cmd, val as c_long, std::ptr::null_mut())) == 0 {
        raise!(ValueError, "Unsupported protocol version 0x{:x}", n);
    }
    Ok(())
}

fn get_min(o: &PyObject) -> PyResult {
    get_proto_version(o, SSL_CTRL_GET_MIN_PROTO_VERSION, -2)
}
fn set_min(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    set_proto_version(o, v, false)
}
fn get_max(o: &PyObject) -> PyResult {
    get_proto_version(o, SSL_CTRL_GET_MAX_PROTO_VERSION, -1)
}
fn set_max(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    set_proto_version(o, v, true)
}

fn get_security_level(o: &PyObject) -> PyResult {
    Ok(new_int(sl!(SSL_CTX_get_security_level(ctx(o).ctx)) as i64))
}

fn get_num_tickets(o: &PyObject) -> PyResult {
    Ok(new_int(sl!(SSL_CTX_get_num_tickets(ctx(o).ctx)) as i64))
}

fn set_num_tickets(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(AttributeError, "cannot delete attribute"),
    };
    let n = rpy_runtime::abstract_::as_i64(v)?;
    if n < 0 {
        raise!(ValueError, "value must be non-negative");
    }
    if ctx(o).protocol != 0x11 {
        raise!(ValueError, "SSLContext is not a server context.");
    }
    if sl!(SSL_CTX_set_num_tickets(ctx(o).ctx, n as usize)) != 1 {
        raise!(ValueError, "failed to set num tickets.");
    }
    Ok(())
}

fn get_pha(o: &PyObject) -> PyResult {
    Ok(rpy_runtime::types::bool_::new_bool(ctx(o).post_handshake_auth))
}

fn set_pha(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(AttributeError, "cannot delete attribute"),
    };
    let b = rpy_runtime::abstract_::is_true(v)?;
    ctx(o).post_handshake_auth = b;
    Ok(())
}

fn get_sni(o: &PyObject) -> PyResult {
    Ok(ctx(o).sni_callback.clone().unwrap_or_else(none))
}

fn set_sni(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let c = ctx(o);
    if v.is_none() {
        raise!(AttributeError, "cannot delete attribute");
    }
    if c.protocol == 0x10 && v.map_or(false, |x| !is_none(x)) {
        raise!(ValueError, "sni_callback cannot be set on TLS_CLIENT context");
    }
    match v {
        None => c.sni_callback = None,
        Some(v) if is_none(v) => c.sni_callback = None,
        Some(v) => {
            if !rpy_runtime::abstract_::is_callable(v) {
                raise!(TypeError, "not a callable object");
            }
            c.sni_callback = Some(v.to_ref());
        }
    }
    Ok(())
}

fn get_msg_cb(o: &PyObject) -> PyResult {
    Ok(ctx(o).msg_callback.clone().unwrap_or_else(none))
}

fn set_msg_cb(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let c = ctx(o);
    if v.is_none() {
        raise!(AttributeError, "_msg_callback cannot be deleted");
    }
    match v {
        Some(v) if !is_none(v) => {
            if !rpy_runtime::abstract_::is_callable(v) {
                raise!(TypeError, "{} is not callable.", crate::ctypes::repr(v)?);
            }
            c.msg_callback = Some(v.to_ref());
            sl!(SSL_CTX_set_msg_callback(c.ctx, super::socket::msg_cb as *mut c_void));
        }
        _ => {
            c.msg_callback = None;
            sl!(SSL_CTX_set_msg_callback(c.ctx, std::ptr::null_mut()));
        }
    }
    Ok(())
}

fn get_keylog(o: &PyObject) -> PyResult {
    Ok(ctx(o).keylog_filename.clone().unwrap_or_else(none))
}

fn set_keylog(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let c = ctx(o);
    if v.is_none() {
        raise!(AttributeError, "keylog_filename cannot be deleted");
    }
    match v {
        Some(v) if !is_none(v) => {
            // the file must be writable; lines are appended on every key event
            let path = rpy_runtime::modules::posix::path::fspath(v)?;
            let p = if is_bytes(&path) { String::from_utf8_lossy(bytes::bytes_data(&path)).into_owned() } else { str_of(&path) };
            match std::fs::OpenOptions::new().append(true).create(true).open(&p) {
                Err(e) => return Err(rpy_runtime::types::exceptions::os_error_from_errno(e.raw_os_error().unwrap_or(0), Some(v), None)),
                Ok(mut fh) => {
                    // a new (empty) log file starts with a comment line
                    if fh.metadata().map(|m| m.len() == 0).unwrap_or(false) {
                        use std::io::Write;
                        let _ = fh.write_all(b"# TLS secrets log file, generated by OpenSSL / Python\n");
                    }
                }
            }
            c.keylog_filename = Some(v.to_ref());
            sl!(SSL_CTX_set_keylog_callback(c.ctx, keylog_cb as *mut c_void));
        }
        _ => {
            c.keylog_filename = None;
            sl!(SSL_CTX_set_keylog_callback(c.ctx, std::ptr::null_mut()));
        }
    }
    Ok(())
}

unsafe extern "C" fn keylog_cb(ssl: *mut c_void, line: *const c_char) {
    unsafe {
        let g = crate::gil::ensure();
        let sock = sl!(SSL_get_ex_data(ssl, super::socket::SSL_EX_INDEX)) as *mut PyObject;
        if !sock.is_null() {
            let ctxref = super::socket::ss(&*sock).context.clone();
            if let Some(f) = &ctx(&ctxref).keylog_filename {
                if let Ok(p) = rpy_runtime::modules::posix::path::fspath(f) {
                    let path = if is_bytes(&p) { String::from_utf8_lossy(bytes::bytes_data(&p)).into_owned() } else { str_of(&p) };
                    use std::io::Write;
                    if let Ok(mut fh) = std::fs::OpenOptions::new().append(true).create(true).open(path) {
                        let _ = fh.write_all(CStr::from_ptr(line).to_bytes());
                        let _ = fh.write_all(b"\n");
                    }
                } else {
                    rpy_runtime::errors::clear_exc();
                }
            }
        }
        crate::gil::release(g);
    }
}

/// SNI: call `sni_callback(sslobject, server_name, context)`.
unsafe extern "C" fn servername_cb(ssl: *mut c_void, al: *mut c_int, _arg: *mut c_void) -> c_int {
    unsafe {
        let g = crate::gil::ensure();
        let r = (|| -> c_int {
            // the context is reached through the socket, which holds a strong reference
            let sock = sl!(SSL_get_ex_data(ssl, super::socket::SSL_EX_INDEX)) as *mut PyObject;
            if sock.is_null() {
                return 0;
            }
            let ctxref = super::socket::ss(&*sock).context.clone();
            let o = ctxref.as_ptr();
            let cb = match &ctx(&*o).sni_callback {
                Some(cb) => cb.clone(),
                None => return 0,
            };
            // a dead owner (SSLObject/SSLSocket collected) fails the handshake
            let owner = match super::socket::live_owner(&*sock) {
                Some(w) => w,
                None => {
                    *al = 80; // internal_error
                    return 2;
                }
            };
            let name = sl!(SSL_get_servername(ssl, 0));
            let name_obj = if name.is_null() {
                none()
            } else {
                match from_c(capi::PyUnicode_Decode(name, libc::strlen(name) as isize, cs!("ascii"), cs!("strict"))) {
                    Ok(s) => s,
                    Err(_) => {
                        capi::PyErr_WriteUnraisable(o);
                        *al = 80; // internal_error
                        return 2;
                    }
                }
            };
            let ctxobj = (&*o).to_ref();
            match call(&cb, &[&owner, &name_obj, &ctxobj]) {
                Ok(res) => {
                    if is_none(&res) {
                        0
                    } else {
                        match rpy_runtime::abstract_::as_i64(&res) {
                            Ok(v) => {
                                *al = v as c_int;
                                2
                            }
                            Err(_) => {
                                capi::PyErr_WriteUnraisable(cb.as_ptr());
                                *al = 80;
                                2
                            }
                        }
                    }
                }
                Err(_) => {
                    capi::PyErr_WriteUnraisable(cb.as_ptr());
                    *al = 40; // handshake failure
                    2
                }
            }
        })();
        crate::gil::release(g);
        r
    }
}

// ------------------------------------------------------------------ ciphers

fn m_set_ciphers(o: &PyObject, s: &PyObject) -> PyResult {
    if !is_str(s) {
        raise!(TypeError, "set_ciphers() argument must be str, not {}", tname(s));
    }
    let c = cstring(&str_of(s));
    if sl!(SSL_CTX_set_cipher_list(ctx(o).ctx, c.as_ptr())) == 0 {
        cr!(ERR_clear_error());
        return Err(raise_simple(&st().ssl_error, 0, "No cipher can be selected."));
    }
    Ok(none())
}

pub fn cipher_to_dict(cipher: *mut c_void) -> PyResult {
    let d = dict::new_dict();
    let nidname = |nid: c_int| -> PyRef { if nid == 0 { none() } else { new_str(&cstr(cr!(OBJ_nid2ln(nid)))) } };
    dict::set_item_str(&d, "id", &new_int(sl!(SSL_CIPHER_get_id(cipher)) as i64))?;
    dict::set_item_str(&d, "name", &new_str(&cstr(sl!(SSL_CIPHER_get_name(cipher)))))?;
    dict::set_item_str(&d, "protocol", &new_str(&cstr(sl!(SSL_CIPHER_get_version(cipher)))))?;
    let mut buf = [0 as c_char; 512];
    sl!(SSL_CIPHER_description(cipher, buf.as_mut_ptr(), 512));
    let desc = cstr(buf.as_ptr());
    dict::set_item_str(&d, "description", &new_str(desc.trim_end_matches('\n')))?;
    let mut alg = 0;
    let bits = sl!(SSL_CIPHER_get_bits(cipher, &mut alg));
    dict::set_item_str(&d, "strength_bits", &new_int(bits as i64))?;
    dict::set_item_str(&d, "alg_bits", &new_int(alg as i64))?;
    dict::set_item_str(&d, "aead", &rpy_runtime::types::bool_::new_bool(sl!(SSL_CIPHER_is_aead(cipher)) != 0))?;
    dict::set_item_str(&d, "symmetric", &nidname(sl!(SSL_CIPHER_get_cipher_nid(cipher))))?;
    dict::set_item_str(&d, "digest", &nidname(sl!(SSL_CIPHER_get_digest_nid(cipher))))?;
    dict::set_item_str(&d, "kea", &nidname(sl!(SSL_CIPHER_get_kx_nid(cipher))))?;
    dict::set_item_str(&d, "auth", &nidname(sl!(SSL_CIPHER_get_auth_nid(cipher))))?;
    Ok(d)
}

fn m_get_ciphers(o: &PyObject) -> PyResult {
    let ssl = sl!(SSL_new(ctx(o).ctx));
    if ssl.is_null() {
        return Err(raise_from_queue(None, "failed to create SSL"));
    }
    let sk = sl!(SSL_get_ciphers(ssl));
    let l = rpy_runtime::types::list::new_list(Vec::new());
    let n = cr!(OPENSSL_sk_num(sk));
    for i in 0..n {
        let c = cr!(OPENSSL_sk_value(sk, i));
        match cipher_to_dict(c) {
            Ok(d) => rpy_runtime::types::list::append(&l, d),
            Err(e) => {
                sl!(SSL_free(ssl));
                return Err(e);
            }
        }
    }
    sl!(SSL_free(ssl));
    Ok(l)
}

unsafe extern "C" fn alpn_select_cb(_ssl: *mut c_void, out: *mut *const u8, outlen: *mut u8, inp: *const u8, inlen: c_uint, arg: *mut c_void) -> c_int {
    unsafe {
        let o = &*(arg as *mut PyObject);
        let c = ctx(o);
        if c.alpn.is_empty() {
            return 3; // SSL_TLSEXT_ERR_NOACK
        }
        let mut sel: *mut u8 = std::ptr::null_mut();
        let r = sl!(SSL_select_next_proto(&mut sel, outlen, c.alpn.as_ptr(), c.alpn.len() as c_uint, inp, inlen));
        if r != 1 {
            return 3;
        }
        *out = sel;
        0
    }
}

fn m_set_alpn_protocols(o: &PyObject, protos: &PyObject) -> PyResult {
    let mut view: Py_buffer = unsafe { std::mem::zeroed() };
    unsafe {
        if capi::PyObject_GetBuffer(protos.as_ptr(), &mut view, 0) < 0 {
            return Err(PyErr);
        }
    }
    let data = unsafe { std::slice::from_raw_parts(view.buf as *const u8, view.len as usize) }.to_vec();
    unsafe { capi::PyBuffer_Release(&mut view) };
    if data.len() > u32::MAX as usize {
        raise!(OverflowError, "protocols longer than {} bytes", u32::MAX);
    }
    let c = ctx(o);
    c.alpn = data;
    if sl!(SSL_CTX_set_alpn_protos(c.ctx, c.alpn.as_ptr(), c.alpn.len() as c_uint)) != 0 {
        raise!(MemoryError, "failed to set ALPN protocols");
    }
    sl!(SSL_CTX_set_alpn_select_cb(c.ctx, alpn_select_cb as *mut c_void, o.as_ptr() as *mut c_void));
    Ok(none())
}

// ------------------------------------------------------------------ certificates and keys

struct PwInfo {
    callable: Option<PyRef>,
    data: Vec<u8>,
    error: bool,
}

fn password_bytes(v: &PyObject, from_callback: bool) -> PyResult<Vec<u8>> {
    if is_str(v) {
        return Ok(str_of(v).into_bytes());
    }
    if is_bytes(v) {
        return Ok(bytes::bytes_data(v).to_vec());
    }
    if rpy_runtime::types::bytearray::is_bytearray(v) {
        return Ok(rpy_runtime::types::bytearray::bytearray_data(v).to_vec());
    }
    if from_callback {
        raise!(TypeError, "password callback must return a string");
    }
    raise!(TypeError, "password should be a string or callable")
}

unsafe extern "C" fn password_cb(buf: *mut c_char, size: c_int, _rw: c_int, userdata: *mut c_void) -> c_int {
    unsafe {
        let info = &mut *(userdata as *mut PwInfo);
        if info.error {
            return -1;
        }
        let g = crate::gil::ensure();
        let r = (|| -> PyResult<Vec<u8>> {
            if let Some(cb) = &info.callable {
                let v = call(cb, &[])?;
                return password_bytes(&v, true);
            }
            Ok(info.data.clone())
        })();
        let out = match r {
            Ok(pw) => {
                if pw.len() > size as usize {
                    let _ = err!(ValueError, "password cannot be longer than {} bytes", size);
                    info.error = true;
                    -1
                } else {
                    std::ptr::copy_nonoverlapping(pw.as_ptr(), buf as *mut u8, pw.len());
                    pw.len() as c_int
                }
            }
            Err(_) => {
                info.error = true;
                -1
            }
        };
        crate::gil::release(g);
        out
    }
}

fn path_cstring(p: &PyObject) -> PyResult<CString> {
    let path = rpy_runtime::modules::posix::path::fspath(p)?;
    let b = if is_bytes(&path) {
        bytes::bytes_data(&path).to_vec()
    } else {
        let e = from_c(unsafe { capi::PyUnicode_EncodeFSDefault(path.as_ptr()) })?;
        bytes::bytes_data(&e).to_vec()
    };
    CString::new(b).map_err(|_| err!(ValueError, "embedded null byte"))
}

fn raise_file_or_ssl(path_obj: &PyObject) -> PyErr {
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    let e = cr!(ERR_peek_last_error()) as u64;
    if err_get_lib(e) == 2 || (e == 0 && errno != 0) || errno == libc::ENOENT {
        cr!(ERR_clear_error());
        let en = if errno != 0 { errno } else { libc::ENOENT };
        return rpy_runtime::types::exceptions::os_error_from_errno(en, Some(path_obj), None);
    }
    raise_from_queue(None, "unknown error")
}

fn m_load_cert_chain(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("load_cert_chain", &["certfile", "keyfile", "password"], 0, 1);
    let [certfile, keyfile, password] = rpy_runtime::args::parse::<3>(&P, args, kw)?;
    let certfile = certfile.unwrap();
    let keyfile = keyfile.filter(|k| !is_none(k));
    let c = ctx(o).ctx;
    if !is_str(certfile) && !is_bytes(certfile) && getattr_opt(certfile, "__fspath__")?.is_none() {
        raise!(TypeError, "certfile should be a valid filesystem path");
    }
    if let Some(k) = keyfile {
        if !is_str(k) && !is_bytes(k) && getattr_opt(k, "__fspath__")?.is_none() {
            raise!(TypeError, "keyfile should be a valid filesystem path");
        }
    }
    let mut info = Box::new(PwInfo { callable: None, data: Vec::new(), error: false });
    if let Some(p) = password.filter(|p| !is_none(p)) {
        if rpy_runtime::abstract_::is_callable(p) {
            info.callable = Some(p.to_ref());
        } else {
            info.data = password_bytes(p, false)?;
            if info.data.len() > i32::MAX as usize {
                raise!(ValueError, "password cannot be longer than {} bytes", i32::MAX);
            }
        }
    }
    let infop = &mut *info as *mut PwInfo as *mut c_void;
    sl!(SSL_CTX_set_default_passwd_cb(c, password_cb as *mut c_void));
    sl!(SSL_CTX_set_default_passwd_cb_userdata(c, infop));
    let restore = || {
        sl!(SSL_CTX_set_default_passwd_cb(c, std::ptr::null_mut()));
        sl!(SSL_CTX_set_default_passwd_cb_userdata(c, std::ptr::null_mut()));
    };
    let certpath = path_cstring(certfile)?;
    unsafe { *libc::__errno_location() = 0 };
    let r = sl!(SSL_CTX_use_certificate_chain_file(c, certpath.as_ptr()));
    if r != 1 {
        restore();
        if info.error {
            cr!(ERR_clear_error());
            return Err(PyErr);
        }
        return Err(raise_file_or_ssl(certfile));
    }
    let keyobj = keyfile.unwrap_or(certfile);
    let keypath = path_cstring(keyobj)?;
    unsafe { *libc::__errno_location() = 0 };
    let r = sl!(SSL_CTX_use_PrivateKey_file(c, keypath.as_ptr(), 1));
    if info.error {
        restore();
        cr!(ERR_clear_error());
        return Err(PyErr);
    }
    if r != 1 {
        restore();
        if info.error {
            cr!(ERR_clear_error());
            return Err(PyErr);
        }
        return Err(raise_file_or_ssl(keyobj));
    }
    if sl!(SSL_CTX_check_private_key(c)) != 1 {
        restore();
        return Err(raise_from_queue(None, "key values mismatch"));
    }
    restore();
    Ok(none())
}

fn add_cadata(c: *mut c_void, data: &[u8], pem: bool) -> PyResult<()> {
    let store = sl!(SSL_CTX_get_cert_store(c));
    let bio = cr!(BIO_new_mem_buf(data.as_ptr() as *const c_void, data.len() as c_int));
    let mut loaded = 0;
    let mut der_pos: *const u8 = data.as_ptr();
    let der_end = unsafe { data.as_ptr().add(data.len()) };
    loop {
        let x = if pem {
            cr!(PEM_read_bio_X509(bio, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()))
        } else {
            let remaining = der_end as usize - der_pos as usize;
            if remaining == 0 {
                std::ptr::null_mut()
            } else {
                cr!(d2i_X509(std::ptr::null_mut(), &mut der_pos, remaining as c_long))
            }
        };
        if x.is_null() {
            if loaded > 0 {
                // trailing data that is not a certificate is an error, but running out of
                // PEM blocks is the normal end
                let trailing = if pem {
                    let e = cr!(ERR_peek_last_error()) as u64;
                    e != 0 && !(err_get_lib(e) == 9 && err_get_reason(e) == 108)
                } else {
                    der_pos != der_end
                };
                if trailing {
                    cr!(BIO_free(bio));
                    return Err(raise_from_queue(None, "cadata contains invalid data"));
                }
            }
            break;
        }
        let r = cr!(X509_STORE_add_cert(store, x));
        cr!(X509_free(x));
        if r == 0 {
            let e = cr!(ERR_peek_last_error()) as u64;
            // an already present certificate is fine
            if err_get_lib(e) == 11 && err_get_reason(e) == 101 {
                cr!(ERR_clear_error());
            } else {
                cr!(BIO_free(bio));
                return Err(raise_from_queue(None, "failed to add certificate"));
            }
        }
        loaded += 1;
    }
    cr!(BIO_free(bio));
    if loaded == 0 {
        cr!(ERR_clear_error());
        let msg = if pem { "no start line: cadata does not contain a certificate" } else { "not enough data: cadata does not contain a certificate" };
        return Err(raise_simple(&st().ssl_error, 0, msg));
    }
    // trailing garbage after valid PEM certificates is ignored
    cr!(ERR_clear_error());
    Ok(())
}

fn m_load_verify_locations(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("load_verify_locations", &["cafile", "capath", "cadata"], 0, 0);
    let [cafile, capath, cadata] = rpy_runtime::args::parse::<3>(&P, args, kw)?;
    let cafile = cafile.filter(|x| !is_none(x));
    let capath = capath.filter(|x| !is_none(x));
    let cadata = cadata.filter(|x| !is_none(x));
    if cafile.is_none() && capath.is_none() && cadata.is_none() {
        raise!(TypeError, "cafile, capath and cadata cannot be all omitted");
    }
    let check_path = |p: &PyObject, what: &str| -> PyResult<()> {
        if !is_str(p) && !is_bytes(p) && getattr_opt(p, "__fspath__")?.is_none() {
            raise!(TypeError, "{} should be a valid filesystem path", what);
        }
        Ok(())
    };
    if let Some(f) = cafile {
        check_path(f, "cafile")?;
    }
    if let Some(p) = capath {
        check_path(p, "capath")?;
    }
    let c = ctx(o).ctx;
    if let Some(d) = cadata {
        if is_str(d) {
            let s = str_of(d);
            if !s.is_ascii() {
                raise!(TypeError, "cadata should be an ASCII string or a bytes-like object");
            }
            add_cadata(c, s.as_bytes(), true)?;
        } else if unsafe { capi::PyObject_CheckBuffer(d.as_ptr()) } != 0 {
            let mut view: Py_buffer = unsafe { std::mem::zeroed() };
            unsafe {
                if capi::PyObject_GetBuffer(d.as_ptr(), &mut view, 0) < 0 {
                    return Err(PyErr);
                }
            }
            let data = unsafe { std::slice::from_raw_parts(view.buf as *const u8, view.len as usize) }.to_vec();
            unsafe { capi::PyBuffer_Release(&mut view) };
            add_cadata(c, &data, false)?;
        } else {
            raise!(TypeError, "cadata should be an ASCII string or a bytes-like object");
        }
    }
    if cafile.is_some() || capath.is_some() {
        let f = match cafile {
            Some(f) => Some(path_cstring(f)?),
            None => None,
        };
        let p = match capath {
            Some(p) => Some(path_cstring(p)?),
            None => None,
        };
        unsafe { *libc::__errno_location() = 0 };
        let r = sl!(SSL_CTX_load_verify_locations(c, f.as_ref().map_or(std::ptr::null(), |x| x.as_ptr()), p.as_ref().map_or(std::ptr::null(), |x| x.as_ptr())));
        if r != 1 {
            return Err(raise_file_or_ssl(cafile.or(capath).unwrap()));
        }
    }
    Ok(none())
}

fn m_load_dh_params(o: &PyObject, path: &PyObject) -> PyResult {
    let p = path_cstring(path)?;
    let bio = cr!(BIO_new_file(p.as_ptr(), cs!("r")));
    if bio.is_null() {
        cr!(ERR_clear_error());
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::ENOENT);
        return Err(rpy_runtime::types::exceptions::os_error_from_errno(errno, Some(path), None));
    }
    let dh = cr!(PEM_read_bio_DHparams(bio, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()));
    cr!(BIO_free(bio));
    if dh.is_null() {
        return Err(raise_from_queue(None, "failed to load DH parameters"));
    }
    let r = sl!(SSL_CTX_ctrl(ctx(o).ctx, SSL_CTRL_SET_TMP_DH, 0, dh));
    cr!(DH_free(dh));
    if r != 1 {
        return Err(raise_from_queue(None, "failed to set DH parameters"));
    }
    Ok(none())
}

fn m_set_default_verify_paths(o: &PyObject) -> PyResult {
    if sl!(SSL_CTX_set_default_verify_paths(ctx(o).ctx)) != 1 {
        return Err(raise_from_queue(None, "failed to set default verify paths"));
    }
    Ok(none())
}

fn store_objects(o: &PyObject) -> Vec<(c_int, *mut c_void)> {
    let store = sl!(SSL_CTX_get_cert_store(ctx(o).ctx));
    let objs = cr!(X509_STORE_get0_objects(store));
    let n = cr!(OPENSSL_sk_num(objs));
    (0..n)
        .map(|i| {
            let ob = cr!(OPENSSL_sk_value(objs, i));
            (cr!(X509_OBJECT_get_type(ob)), ob)
        })
        .collect()
}

fn m_cert_store_stats(o: &PyObject) -> PyResult {
    let (mut x509, mut crl, mut ca) = (0, 0, 0);
    for (t, ob) in store_objects(o) {
        match t {
            1 => {
                x509 += 1;
                if cr!(X509_check_ca(cr!(X509_OBJECT_get0_X509(ob)))) != 0 {
                    ca += 1;
                }
            }
            2 => crl += 1,
            _ => {}
        }
    }
    let d = dict::new_dict();
    dict::set_item_str(&d, "x509", &new_int(x509))?;
    dict::set_item_str(&d, "crl", &new_int(crl))?;
    dict::set_item_str(&d, "x509_ca", &new_int(ca))?;
    Ok(d)
}

fn m_get_ca_certs(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("get_ca_certs", &["binary_form"], 0, 0);
    let [binary] = rpy_runtime::args::parse::<1>(&P, args, kw)?;
    let binary = match binary {
        Some(b) => rpy_runtime::abstract_::is_true(b)?,
        None => false,
    };
    let l = rpy_runtime::types::list::new_list(Vec::new());
    for (t, ob) in store_objects(o) {
        if t != 1 {
            continue;
        }
        let x = cr!(X509_OBJECT_get0_X509(ob));
        if cr!(X509_check_ca(x)) == 0 {
            continue;
        }
        let v = if binary { cert_der(x)? } else { decode_certificate(x)? };
        rpy_runtime::types::list::append(&l, v);
    }
    Ok(l)
}

fn m_set_ecdh_curve(o: &PyObject, name: &PyObject) -> PyResult {
    let s = if is_str(name) {
        str_of(name)
    } else if is_bytes(name) {
        String::from_utf8_lossy(bytes::bytes_data(name)).into_owned()
    } else {
        raise!(TypeError, "set_ecdh_curve() argument must be str or bytes, not {}", tname(name));
    };
    let c = cstring(&s);
    let nid = cr!(OBJ_sn2nid(c.as_ptr()));
    if nid == 0 {
        raise!(ValueError, "unknown elliptic curve name {}", crate::ctypes::repr(name)?);
    }
    if sl!(SSL_CTX_ctrl(ctx(o).ctx, SSL_CTRL_SET_GROUPS_LIST, 0, c.as_ptr() as *mut c_void)) != 1 {
        return Err(raise_from_queue(None, "failed to set curve"));
    }
    Ok(none())
}

fn m_session_stats(o: &PyObject) -> PyResult {
    let d = dict::new_dict();
    let c = ctx(o).ctx;
    for (name, cmd) in [
        ("number", 20),
        ("connect", 21),
        ("connect_good", 22),
        ("connect_renegotiate", 23),
        ("accept", 24),
        ("accept_good", 25),
        ("accept_renegotiate", 26),
        ("accept", 24),
        ("hits", 27),
        ("misses", 29),
        ("timeouts", 30),
        ("cache_full", 31),
    ] {
        let v = sl!(SSL_CTX_ctrl(c, cmd, 0, std::ptr::null_mut()));
        dict::set_item_str(&d, name, &new_int(v as i64))?;
    }
    Ok(d)
}

fn ctrl_str(o: &PyObject, cmd: c_int, s: &PyObject, what: &str) -> PyResult {
    if !is_str(s) {
        raise!(TypeError, "{}() argument must be str, not {}", what, tname(s));
    }
    let c = cstring(&str_of(s));
    if sl!(SSL_CTX_ctrl(ctx(o).ctx, cmd, 0, c.as_ptr() as *mut c_void)) != 1 {
        return Err(raise_from_queue(None, &format!("{} failed", what)));
    }
    Ok(none())
}

fn m_set_ciphersuites(o: &PyObject, s: &PyObject) -> PyResult {
    if !is_str(s) {
        raise!(TypeError, "set_ciphersuites() argument must be str, not {}", tname(s));
    }
    let c = cstring(&str_of(s));
    if sl!(SSL_CTX_set_ciphersuites(ctx(o).ctx, c.as_ptr())) == 0 {
        cr!(ERR_clear_error());
        return Err(raise_simple(&st().ssl_error, 0, "No cipher suite can be selected."));
    }
    Ok(none())
}

fn m_set_groups(o: &PyObject, s: &PyObject) -> PyResult {
    ctrl_str(o, SSL_CTRL_SET_GROUPS_LIST, s, "set_groups")
}

fn m_set_client_sigalgs(o: &PyObject, s: &PyObject) -> PyResult {
    ctrl_str(o, 102, s, "set_client_sigalgs")
}

fn m_set_server_sigalgs(o: &PyObject, s: &PyObject) -> PyResult {
    ctrl_str(o, 98, s, "set_server_sigalgs")
}

fn m_psk_unsupported(_o: &PyObject, _args: &[&PyObject], _kw: Option<&PyObject>) -> PyResult {
    raise!(NotImplementedError, "TLS-PSK is not supported by this build")
}

fn wrap_args(args: &[&PyObject], kw: Option<&PyObject>, bio: bool) -> PyResult<Vec<Option<PyRef>>> {
    if bio {
        static P: rpy_runtime::args::Params =
            rpy_runtime::args::Params::new("_wrap_bio", &["incoming", "outgoing", "server_side", "server_hostname", "owner", "session"], 0, 3).kwonly(2);
        let r = rpy_runtime::args::parse::<6>(&P, args, kw)?;
        Ok(r.iter().map(|x| x.map(|y| y.to_ref())).collect())
    } else {
        static P: rpy_runtime::args::Params =
            rpy_runtime::args::Params::new("_wrap_socket", &["sock", "server_side", "server_hostname", "owner", "session"], 0, 2).kwonly(2);
        let r = rpy_runtime::args::parse::<5>(&P, args, kw)?;
        Ok(r.iter().map(|x| x.map(|y| y.to_ref())).collect())
    }
}

fn m_wrap_socket(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let a = wrap_args(args, kw, false)?;
    let sock = a[0].clone().unwrap();
    let server_side = rpy_runtime::abstract_::is_true(a[1].as_ref().unwrap())?;
    super::socket::new_sslsocket(o, super::socket::Transport::Sock(sock), server_side, a[2].clone(), a[3].clone(), a[4].clone())
}

fn m_wrap_bio(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let a = wrap_args(args, kw, true)?;
    let inc = a[0].clone().unwrap();
    let out = a[1].clone().unwrap();
    if !is_membio(&inc) || !is_membio(&out) {
        raise!(TypeError, "_wrap_bio() arguments 1 and 2 must be MemoryBIO objects");
    }
    let server_side = rpy_runtime::abstract_::is_true(a[2].as_ref().unwrap())?;
    super::socket::new_sslsocket(o, super::socket::Transport::Bio(inc, out), server_side, a[3].clone(), a[4].clone(), a[5].clone())
}

pub fn init_type() -> PyResult<()> {
    unsafe {
        let t = context_type();
        fc::init_robj_type::<Ctx>(t, c"_ssl._SSLContext", Py_TPFLAGS_BASETYPE);
        (*t).tp_new = Some(ctx_new);
        (*t).tp_methods = rpy_runtime::mdefs![
            ("_wrap_socket", FASTKW, m_wrap_socket),
            ("_wrap_bio", FASTKW, m_wrap_bio),
            ("set_ciphers", O, m_set_ciphers),
            ("set_ciphersuites", O, m_set_ciphersuites),
            ("set_groups", O, m_set_groups),
            ("set_client_sigalgs", O, m_set_client_sigalgs),
            ("set_server_sigalgs", O, m_set_server_sigalgs),
            ("get_ciphers", NOARGS, m_get_ciphers),
            ("_set_alpn_protocols", O, m_set_alpn_protocols),
            ("load_cert_chain", FASTKW, m_load_cert_chain),
            ("load_verify_locations", FASTKW, m_load_verify_locations),
            ("load_dh_params", O, m_load_dh_params),
            ("set_default_verify_paths", NOARGS, m_set_default_verify_paths),
            ("cert_store_stats", NOARGS, m_cert_store_stats),
            ("get_ca_certs", FASTKW, m_get_ca_certs),
            ("set_ecdh_curve", O, m_set_ecdh_curve),
            ("session_stats", NOARGS, m_session_stats),
            ("set_psk_client_callback", FASTKW, m_psk_unsupported),
            ("set_psk_server_callback", FASTKW, m_psk_unsupported),
        ];
        (*t).tp_getset = rpy_runtime::getset_table![
            ("check_hostname", get_check_hostname, set_check_hostname),
            ("_host_flags", get_host_flags, set_host_flags),
            ("minimum_version", get_min, set_min),
            ("maximum_version", get_max, set_max),
            ("keylog_filename", get_keylog, set_keylog),
            ("_msg_callback", get_msg_cb, set_msg_cb),
            ("sni_callback", get_sni, set_sni),
            ("num_tickets", get_num_tickets, set_num_tickets),
            ("options", get_options, set_options),
            ("post_handshake_auth", get_pha, set_pha),
            ("protocol", get_protocol),
            ("verify_flags", get_verify_flags, set_verify_flags),
            ("verify_mode", get_verify_mode, set_verify_mode),
            ("security_level", get_security_level),
        ];
        rpy_runtime::types::typeobject::type_ready(t)?;
    }
    Ok(())
}
