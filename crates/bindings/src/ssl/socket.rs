//! _ssl._SSLSocket: a TLS connection over a socket file descriptor or a pair of
//! MemoryBIO objects.

use super::context::{ctx, is_context};
use super::*;

pub enum Transport {
    Sock(PyRef),
    Bio(PyRef, PyRef),
}

pub struct SslSock {
    pub ssl: *mut c_void,
    pub context: PyRef,
    /// weak reference to the Python socket (socket transport only)
    pub sock: Option<PyRef>,
    pub incoming: Option<PyRef>,
    pub outgoing: Option<PyRef>,
    pub server_side: bool,
    pub server_hostname: Option<PyRef>,
    pub owner: Option<PyRef>,
    pub shutdown_seen_zero: bool,
    pub pending_exc: Option<PyRef>,
}

impl Trav for SslSock {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        for o in [Some(&self.context), self.sock.as_ref(), self.incoming.as_ref(), self.outgoing.as_ref(), self.server_hostname.as_ref(), self.owner.as_ref()] {
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

impl Drop for SslSock {
    fn drop(&mut self) {
        if !self.ssl.is_null() {
            sl!(SSL_free(self.ssl));
        }
    }
}

pub static mut SOCKET_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub const SSL_EX_INDEX: c_int = 0;

pub fn socket_type() -> *mut PyTypeObject {
    &raw mut SOCKET_TYPE
}

pub fn ss(o: &PyObject) -> &'static mut SslSock {
    unsafe { &mut *(fc::rs::<SslSock>(o) as *mut SslSock) }
}

fn weakref(o: &PyObject) -> PyResult {
    from_c(unsafe { capi::PyWeakref_NewRef(o.as_ptr(), std::ptr::null_mut()) })
}

fn deref_weak(w: &PyObject) -> Option<PyRef> {
    match call(w, &[]) {
        Ok(r) if !is_none(&r) => Some(r),
        Ok(_) => None,
        Err(_) => {
            rpy_runtime::errors::clear_exc();
            None
        }
    }
}

/// The owner object (SSLSocket/SSLObject) or the _SSLSocket itself.
pub fn owner_or_self(o: &PyObject) -> PyRef {
    match &ss(o).owner {
        Some(w) => deref_weak(w).unwrap_or_else(|| o.to_ref()),
        None => o.to_ref(),
    }
}

fn encode_hostname(h: &PyObject) -> PyResult<Vec<u8>> {
    if is_bytes(h) {
        return Ok(bytes::bytes_data(h).to_vec());
    }
    if !is_str(h) {
        raise!(TypeError, "server_hostname must be str, not {}", tname(h));
    }
    let s = str_of(h);
    if s.is_ascii() {
        return Ok(s.into_bytes());
    }
    let enc = rpy_runtime::abstract_::call_method_str(h, "encode", &[&new_str("idna")])?;
    Ok(bytes::bytes_data(&enc).to_vec())
}

pub fn new_sslsocket(
    ctxobj: &PyObject,
    transport: Transport,
    server_side: bool,
    server_hostname: Option<PyRef>,
    owner: Option<PyRef>,
    session: Option<PyRef>,
) -> PyResult {
    let c = ctx(ctxobj);
    let server_hostname = server_hostname.filter(|h| !is_none(h));
    if server_side && c.protocol == 0x10 {
        return Err(raise_simple(&st().ssl_error, 0, "Cannot create a server socket with a PROTOCOL_TLS_CLIENT context"));
    }
    if !server_side && c.protocol == 0x11 {
        return Err(raise_simple(&st().ssl_error, 0, "Cannot create a client socket with a PROTOCOL_TLS_SERVER context"));
    }
    let host = match &server_hostname {
        Some(h) => {
            let b = encode_hostname(h)?;
            if b.is_empty() || b[0] == b'.' {
                raise!(ValueError, "server_hostname cannot be an empty string or start with a leading dot.");
            }
            if b.contains(&0) {
                raise!(ValueError, "server_hostname cannot contain null characters");
            }
            Some(b)
        }
        None => None,
    };
    let ssl = sl!(SSL_new(c.ctx));
    if ssl.is_null() {
        return Err(raise_from_queue(None, "failed to create SSL"));
    }
    sl!(SSL_ctrl(ssl, SSL_CTRL_MODE, SSL_MODE_ENABLE_PARTIAL_WRITE | SSL_MODE_ACCEPT_MOVING_WRITE_BUFFER | SSL_MODE_AUTO_RETRY, std::ptr::null_mut()));
    if c.post_handshake_auth && !server_side {
        sl!(SSL_set_post_handshake_auth(ssl, 1));
    }
    if server_side && c.post_handshake_auth {
        let m = sl!(SSL_get_verify_mode(ssl));
        if m != SSL_VERIFY_NONE {
            sl!(SSL_set_verify(ssl, m | SSL_VERIFY_POST_HANDSHAKE, std::ptr::null_mut()));
        }
    }
    let mut sock_weak = None;
    let (mut incoming, mut outgoing) = (None, None);
    match transport {
        Transport::Sock(sock) => {
            let fd = rpy_runtime::abstract_::call_method_str(&sock, "fileno", &[])?;
            let fd = rpy_runtime::abstract_::as_i64(&fd)?;
            sl!(SSL_set_fd(ssl, fd as c_int));
            sock_weak = Some(weakref(&sock)?);
        }
        Transport::Bio(inc, out) => {
            let (r, w) = (membio(&inc).bio, membio(&out).bio);
            cr!(BIO_up_ref(r));
            cr!(BIO_up_ref(w));
            sl!(SSL_set_bio(ssl, r, w));
            incoming = Some(inc);
            outgoing = Some(out);
        }
    }
    if let Some(h) = &host {
        let s = String::from_utf8_lossy(h).into_owned();
        let ip: Option<std::net::IpAddr> = s.parse().ok();
        let param = sl!(SSL_get0_param(ssl));
        match ip {
            Some(addr) => {
                if c.check_hostname {
                    let raw: Vec<u8> = match addr {
                        std::net::IpAddr::V4(a) => a.octets().to_vec(),
                        std::net::IpAddr::V6(a) => a.octets().to_vec(),
                    };
                    cr!(X509_VERIFY_PARAM_set1_ip(param, raw.as_ptr(), raw.len()));
                }
            }
            None => {
                let ch = CString::new(h.clone()).unwrap();
                sl!(SSL_ctrl(ssl, SSL_CTRL_SET_TLSEXT_HOSTNAME, 0, ch.as_ptr() as *mut c_void));
                if c.check_hostname {
                    cr!(X509_VERIFY_PARAM_set1_host(param, ch.as_ptr(), h.len()));
                }
            }
        }
    }
    if server_side {
        sl!(SSL_set_accept_state(ssl));
    } else {
        sl!(SSL_set_connect_state(ssl));
    }
    let owner_weak = match owner {
        Some(o) if !is_none(&o) => Some(weakref(&o)?),
        _ => None,
    };
    let obj = fc::robj_new(
        socket_type(),
        SslSock {
            ssl,
            context: ctxobj.to_ref(),
            sock: sock_weak,
            incoming,
            outgoing,
            server_side,
            server_hostname,
            owner: owner_weak,
            shutdown_seen_zero: false,
            pending_exc: None,
        },
    )?;
    sl!(SSL_set_ex_data(ssl, SSL_EX_INDEX, obj.as_ptr() as *mut c_void));
    if let Some(s) = session.filter(|s| !is_none(s)) {
        set_session(&obj, Some(&s))?;
    }
    Ok(obj)
}

// ------------------------------------------------------------------ I/O helpers

enum Wait {
    Ready,
    Timeout,
    NoSocket,
}

/// The socket's timeout: None = blocking, Some(0.0) = non-blocking.
fn sock_info(o: &PyObject) -> PyResult<(Option<PyRef>, Option<f64>)> {
    let s = ss(o);
    let sock = match &s.sock {
        None => return Ok((None, Some(0.0))), // BIO: never block
        Some(w) => match deref_weak(w) {
            Some(sk) => sk,
            None => return Err(raise_simple(&st().ssl_error, 0, "Underlying socket connection gone")),
        },
    };
    let fd = rpy_runtime::abstract_::call_method_str(&sock, "fileno", &[])?;
    if rpy_runtime::abstract_::as_i64(&fd)? < 0 {
        return Err(raise_simple(&st().ssl_error, 0, "Underlying socket has been closed."));
    }
    let t = rpy_runtime::abstract_::call_method_str(&sock, "gettimeout", &[])?;
    let timeout = if is_none(&t) { None } else { Some(rpy_runtime::abstract_::float_value(&t)?) };
    Ok((Some(sock), timeout))
}

fn wait_fd(sock: &PyObject, writing: bool, timeout: Option<f64>) -> PyResult<Wait> {
    let fdo = rpy_runtime::abstract_::call_method_str(sock, "fileno", &[])?;
    let fd = rpy_runtime::abstract_::as_i64(&fdo)? as c_int;
    if fd < 0 {
        return Ok(Wait::NoSocket);
    }
    let ms = match timeout {
        None => -1,
        Some(t) => (t * 1000.0).ceil() as c_int,
    };
    let mut pfd = libc::pollfd { fd, events: if writing { libc::POLLOUT } else { libc::POLLIN }, revents: 0 };
    let r = rpy_runtime::threads::blocking(|| unsafe { libc::poll(&mut pfd, 1, ms) });
    if r == 0 {
        return Ok(Wait::Timeout);
    }
    if r < 0 {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if errno == libc::EINTR {
            if unsafe { capi::PyErr_CheckSignals() } < 0 {
                return Err(PyErr);
            }
            return Ok(Wait::Ready);
        }
        return Err(rpy_runtime::types::exceptions::os_error_from_errno(errno, None, None));
    }
    Ok(Wait::Ready)
}

fn timeout_error(what: &str) -> PyErr {
    set_err_simple_type(rpy_runtime::types::exceptions::TimeoutError(), &format!("The {} operation timed out", what))
}

fn set_err_simple_type(t: *mut PyTypeObject, msg: &str) -> PyErr {
    rpy_runtime::errors::new_err(t, msg)
}

/// Raise the Python exception for an SSL_get_error() result.
fn raise_ssl_err(o: &PyObject, ret: c_int, err: c_int) -> PyErr {
    let s = st();
    if let Some(e) = ss(o).pending_exc.take() {
        rpy_runtime::errors::set_exc(e);
        return PyErr;
    }
    let ssl = ss(o).ssl;
    match err {
        SSL_ERROR_ZERO_RETURN => raise_simple(&s.zero_return_error, 6, "TLS/SSL connection has been closed (EOF) (_ssl.c:1000)"),
        SSL_ERROR_WANT_READ => raise_simple(&s.want_read_error, 2, "The operation did not complete (read) (_ssl.c:1000)"),
        SSL_ERROR_WANT_WRITE => raise_simple(&s.want_write_error, 3, "The operation did not complete (write) (_ssl.c:1000)"),
        SSL_ERROR_WANT_X509_LOOKUP => raise_simple(&s.ssl_error, 4, "The operation did not complete (X509 lookup) (_ssl.c:1000)"),
        SSL_ERROR_WANT_CONNECT => raise_simple(&s.ssl_error, 7, "The operation did not complete (connect) (_ssl.c:1000)"),
        SSL_ERROR_SYSCALL => {
            let e = cr!(ERR_peek_last_error()) as u64;
            if e == 0 {
                let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                if ret == 0 || ss(o).sock.as_ref().map_or(false, |w| deref_weak(w).is_none()) {
                    return raise_simple(&s.eof_error, 8, "EOF occurred in violation of protocol (_ssl.c:1000)");
                }
                if ret == -1 && errno != 0 {
                    cr!(ERR_clear_error());
                    return rpy_runtime::types::exceptions::os_error_from_errno(errno, None, None);
                }
                return raise_simple(&s.syscall_error, 5, "Some I/O error occurred (_ssl.c:1000)");
            }
            cr!(ERR_clear_error());
            raise_errcode(None, e, 5, "Some I/O error occurred", None)
        }
        _ => {
            let e = cr!(ERR_peek_last_error()) as u64;
            cr!(ERR_clear_error());
            // unexpected EOF while reading
            if err_get_lib(e) == 20 && err_get_reason(e) == 294 {
                return raise_simple(&s.eof_error, 8, "EOF occurred in violation of protocol (_ssl.c:1000)");
            }
            // certificate verification failed
            if err_get_lib(e) == 20 && err_get_reason(e) == 134 {
                let code = sl!(SSL_get_verify_result(ssl)) as i64;
                let text = match code {
                    62 => format!(
                        "Hostname mismatch, certificate is not valid for '{}'.",
                        ss(o).server_hostname.as_ref().map(|h| str_of(h)).unwrap_or_default()
                    ),
                    64 => format!(
                        "IP address mismatch, certificate is not valid for '{}'.",
                        ss(o).server_hostname.as_ref().map(|h| str_of(h)).unwrap_or_default()
                    ),
                    _ => cstr(cr!(X509_verify_cert_error_string(code as c_long))),
                };
                return raise_errcode(None, e, 1, "certificate verify failed", Some((code, text)));
            }
            raise_errcode(None, e, 1, "A failure in the SSL library occurred", None)
        }
    }
}

/// Run an OpenSSL operation with the socket's blocking/timeout semantics.
fn run_io(o: &PyObject, what: &str, mut op: impl FnMut(*mut c_void) -> c_int) -> PyResult<c_int> {
    let (sock, timeout) = sock_info(o)?;
    let ssl = ss(o).ssl;
    loop {
        cr!(ERR_clear_error());
        unsafe { *libc::__errno_location() = 0 };
        let ts = unsafe { capi::PyEval_SaveThread() };
        let ret = op(ssl);
        let err = if ret > 0 { SSL_ERROR_NONE } else { sl!(SSL_get_error(ssl, ret)) };
        unsafe { capi::PyEval_RestoreThread(ts) };
        if unsafe { capi::PyErr_CheckSignals() } < 0 {
            return Err(PyErr);
        }
        if let Some(e) = ss(o).pending_exc.take() {
            rpy_runtime::errors::set_exc(e);
            return Err(PyErr);
        }
        if err == SSL_ERROR_NONE {
            return Ok(ret);
        }
        if err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE {
            match (&sock, timeout) {
                (Some(sk), t) if t != Some(0.0) => match wait_fd(sk, err == SSL_ERROR_WANT_WRITE, t)? {
                    Wait::Ready => continue,
                    Wait::Timeout => return Err(timeout_error(what)),
                    Wait::NoSocket => return Err(raise_simple(&st().ssl_error, 0, "Underlying socket has been closed.")),
                },
                _ => {}
            }
        }
        return Err(raise_ssl_err(o, ret, err));
    }
}

// ------------------------------------------------------------------ methods

fn m_do_handshake(o: &PyObject) -> PyResult {
    run_io(o, "handshake", |ssl| sl!(SSL_do_handshake(ssl)))?;
    Ok(none())
}

fn m_read(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 2 {
        raise!(TypeError, "read expected 1 or 2 arguments, got {}", args.len());
    }
    let mut len = rpy_runtime::abstract_::as_i64(args[0])?;
    let buffer = args.get(1).copied().filter(|b| !is_none(b));
    let mut view: Py_buffer = unsafe { std::mem::zeroed() };
    let has_buf = buffer.is_some();
    if let Some(b) = buffer {
        unsafe {
            if capi::PyObject_GetBuffer(b.as_ptr(), &mut view, 0x1) < 0 {
                return Err(PyErr);
            }
        }
        if len <= 0 || len > view.len as i64 {
            len = view.len as i64;
        }
    } else if len < 0 {
        raise!(ValueError, "size should not be negative");
    }
    let result = (|| -> PyResult {
        if len == 0 {
            return Ok(if has_buf { new_int(0) } else { bytes::new_bytes(&[]) });
        }
        let mut tmp: Vec<u8> = if has_buf { Vec::new() } else { vec![0u8; len as usize] };
        let dst: *mut u8 = if has_buf { view.buf as *mut u8 } else { tmp.as_mut_ptr() };
        let mut nread: usize = 0;
        let r = run_io(o, "read", |ssl| sl!(SSL_read_ex(ssl, dst as *mut c_void, len as usize, &mut nread)));
        let count = match r {
            Ok(_) => nread,
            Err(_) => {
                // a clean close by the peer reads as EOF
                let shut = sl!(SSL_get_shutdown(ss(o).ssl));
                if rpy_runtime::errors::exc_matches(st().zero_return_error.as_ptr() as *mut PyTypeObject) && shut & SSL_RECEIVED_SHUTDOWN != 0 {
                    rpy_runtime::errors::clear_exc();
                    0
                } else {
                    return Err(PyErr);
                }
            }
        };
        if has_buf {
            Ok(new_int(count as i64))
        } else {
            tmp.truncate(count);
            Ok(bytes::new_bytes(&tmp))
        }
    })();
    if has_buf {
        unsafe { capi::PyBuffer_Release(&mut view) };
    }
    result
}

fn m_write(o: &PyObject, data: &PyObject) -> PyResult {
    let mut view: Py_buffer = unsafe { std::mem::zeroed() };
    unsafe {
        if capi::PyObject_GetBuffer(data.as_ptr(), &mut view, 0) < 0 {
            return Err(PyErr);
        }
    }
    let buf = view.buf as *const c_void;
    let len = view.len as usize;
    let mut written: usize = 0;
    let r = run_io(o, "write", |ssl| sl!(SSL_write_ex(ssl, buf, len, &mut written)));
    unsafe { capi::PyBuffer_Release(&mut view) };
    r?;
    Ok(new_int(written as i64))
}

fn m_pending(o: &PyObject) -> PyResult {
    Ok(new_int(sl!(SSL_pending(ss(o).ssl)) as i64))
}

fn handshake_done(o: &PyObject) -> bool {
    sl!(SSL_is_init_finished(ss(o).ssl)) != 0
}

fn m_getpeercert(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let binary = if args.is_empty() { false } else { rpy_runtime::abstract_::is_true(args[0])? };
    if !handshake_done(o) {
        raise!(ValueError, "handshake not done yet");
    }
    let x = sl!(SSL_get1_peer_certificate(ss(o).ssl));
    if x.is_null() {
        return Ok(none());
    }
    let r = if binary {
        cert_der(x)
    } else {
        let mode = sl!(SSL_get_verify_mode(ss(o).ssl));
        if mode & SSL_VERIFY_PEER == 0 { Ok(dict::new_dict()) } else { decode_certificate(x) }
    };
    cr!(X509_free(x));
    r
}

fn chain_to_list(sk: *mut c_void, first: Option<*mut c_void>) -> PyResult {
    let l = rpy_runtime::types::list::new_list(Vec::new());
    if let Some(x) = first {
        rpy_runtime::types::list::append(&l, new_certificate(x)?);
    }
    if !sk.is_null() {
        let n = cr!(OPENSSL_sk_num(sk));
        for i in 0..n {
            rpy_runtime::types::list::append(&l, new_certificate(cr!(OPENSSL_sk_value(sk, i)))?);
        }
    }
    Ok(l)
}

fn m_get_verified_chain(o: &PyObject) -> PyResult {
    let sk = sl!(SSL_get0_verified_chain(ss(o).ssl));
    if sk.is_null() {
        return Ok(none());
    }
    chain_to_list(sk, None)
}

fn m_get_unverified_chain(o: &PyObject) -> PyResult {
    let s = ss(o);
    let sk = sl!(SSL_get_peer_cert_chain(s.ssl));
    if sk.is_null() {
        return Ok(none());
    }
    if s.server_side {
        // the server-side chain does not include the peer certificate itself
        let x = sl!(SSL_get1_peer_certificate(s.ssl));
        let r = chain_to_list(sk, if x.is_null() { None } else { Some(x) });
        if !x.is_null() {
            cr!(X509_free(x));
        }
        return r;
    }
    chain_to_list(sk, None)
}

fn cipher_tuple(c: *mut c_void) -> PyRef {
    let name = cstr(sl!(SSL_CIPHER_get_name(c)));
    let version = cstr(sl!(SSL_CIPHER_get_version(c)));
    let bits = sl!(SSL_CIPHER_get_bits(c, std::ptr::null_mut()));
    tuple::new_tuple(vec![new_str(&name), new_str(&version), new_int(bits as i64)])
}

fn m_cipher(o: &PyObject) -> PyResult {
    let c = sl!(SSL_get_current_cipher(ss(o).ssl));
    if c.is_null() {
        return Ok(none());
    }
    Ok(cipher_tuple(c))
}

fn m_shared_ciphers(o: &PyObject) -> PyResult {
    let sk = sl!(SSL_get_client_ciphers(ss(o).ssl));
    if sk.is_null() {
        return Ok(none());
    }
    let n = cr!(OPENSSL_sk_num(sk));
    let mut v = Vec::new();
    for i in 0..n {
        v.push(cipher_tuple(cr!(OPENSSL_sk_value(sk, i))));
    }
    Ok(rpy_runtime::types::list::new_list(v))
}

fn m_version(o: &PyObject) -> PyResult {
    if !handshake_done(o) {
        return Ok(none());
    }
    let v = cstr(sl!(SSL_get_version(ss(o).ssl)));
    if v == "unknown" {
        return Ok(none());
    }
    Ok(new_str(&v))
}

fn m_selected_alpn(o: &PyObject) -> PyResult {
    let mut p: *const u8 = std::ptr::null();
    let mut n: u32 = 0;
    sl!(SSL_get0_alpn_selected(ss(o).ssl, &mut p, &mut n));
    if p.is_null() || n == 0 {
        return Ok(none());
    }
    Ok(new_str(&String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(p, n as usize) })))
}

fn m_compression(_o: &PyObject) -> PyResult {
    Ok(none())
}

fn m_get_channel_binding(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: rpy_runtime::args::Params = rpy_runtime::args::Params::new("get_channel_binding", &["cb_type"], 0, 0);
    let [cb] = rpy_runtime::args::parse::<1>(&P, args, kw)?;
    let cb_type = cb.map(str_of).unwrap_or_else(|| "tls-unique".to_string());
    if cb_type != "tls-unique" {
        raise!(ValueError, "Unsupported channel binding type '{}'", cb_type);
    }
    let s = ss(o);
    let mut buf = [0u8; 128];
    let reused = sl!(SSL_session_reused(s.ssl)) != 0;
    let n = if reused ^ !s.server_side {
        sl!(SSL_get_finished(s.ssl, buf.as_mut_ptr() as *mut c_void, 128))
    } else {
        sl!(SSL_get_peer_finished(s.ssl, buf.as_mut_ptr() as *mut c_void, 128))
    };
    if n == 0 {
        return Ok(none());
    }
    Ok(bytes::new_bytes(&buf[..n]))
}

fn m_verify_client_post_handshake(o: &PyObject) -> PyResult {
    if sl!(SSL_verify_client_post_handshake(ss(o).ssl)) != 1 {
        return Err(raise_from_queue(None, "post-handshake authentication failed"));
    }
    Ok(none())
}

fn m_shutdown(o: &PyObject) -> PyResult {
    let (sock, timeout) = sock_info(o)?;
    let ssl = ss(o).ssl;
    let mut zeros = 0;
    loop {
        cr!(ERR_clear_error());
        let ts = unsafe { capi::PyEval_SaveThread() };
        let ret = sl!(SSL_shutdown(ssl));
        let err = if ret < 0 { sl!(SSL_get_error(ssl, ret)) } else { SSL_ERROR_NONE };
        unsafe { capi::PyEval_RestoreThread(ts) };
        if ret > 0 {
            break;
        }
        if ret == 0 {
            zeros += 1;
            if zeros > 1 {
                break;
            }
            ss(o).shutdown_seen_zero = true;
            continue;
        }
        if err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE {
            if let (Some(sk), t) = (&sock, timeout) {
                if t != Some(0.0) {
                    match wait_fd(sk, err == SSL_ERROR_WANT_WRITE, t)? {
                        Wait::Ready => continue,
                        Wait::Timeout => return Err(timeout_error("read")),
                        Wait::NoSocket => return Err(raise_simple(&st().ssl_error, 0, "Underlying socket has been closed.")),
                    }
                }
            }
        }
        return Err(raise_ssl_err(o, ret, err));
    }
    Ok(sock.unwrap_or_else(none))
}

// ------------------------------------------------------------------ attributes

fn g_context(o: &PyObject) -> PyResult {
    Ok(ss(o).context.clone())
}

fn s_context(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) if is_context(v) => v,
        _ => raise!(TypeError, "The value must be a SSLContext"),
    };
    let s = ss(o);
    sl!(SSL_set_SSL_CTX(s.ssl, ctx(v).ctx));
    s.context = v.to_ref();
    Ok(())
}

fn g_server_side(o: &PyObject) -> PyResult {
    Ok(rpy_runtime::types::bool_::new_bool(ss(o).server_side))
}

fn g_server_hostname(o: &PyObject) -> PyResult {
    Ok(ss(o).server_hostname.clone().unwrap_or_else(none))
}

fn g_owner(o: &PyObject) -> PyResult {
    Ok(match &ss(o).owner {
        Some(w) => deref_weak(w).unwrap_or_else(none),
        None => none(),
    })
}

fn s_owner(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    ss(o).owner = match v {
        Some(v) if !is_none(v) => Some(weakref(v)?),
        _ => None,
    };
    Ok(())
}

fn g_session(o: &PyObject) -> PyResult {
    let sess = sl!(SSL_get1_session(ss(o).ssl));
    if sess.is_null() {
        return Ok(none());
    }
    new_session(sess, ss(o).context.clone())
}

fn set_session(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(AttributeError, "cannot delete attribute"),
    };
    if !is_session(v) {
        raise!(TypeError, "Value is not a SSLSession.");
    }
    let s = ss(o);
    if !session(v).ctx.is(&s.context) {
        raise!(ValueError, "Session refers to a different SSLContext.");
    }
    if s.server_side {
        raise!(ValueError, "Cannot set session for server-side SSLSocket.");
    }
    if handshake_done(o) {
        raise!(ValueError, "Cannot set session after handshake.");
    }
    if sl!(SSL_set_session(s.ssl, session(v).sess)) != 1 {
        return Err(raise_from_queue(None, "failed to set session"));
    }
    Ok(())
}

fn g_session_reused(o: &PyObject) -> PyResult {
    Ok(rpy_runtime::types::bool_::new_bool(sl!(SSL_session_reused(ss(o).ssl)) != 0))
}

fn g_false(_o: &PyObject) -> PyResult {
    Ok(rpy_runtime::types::bool_::new_bool(false))
}

// ------------------------------------------------------------------ message callback

pub unsafe extern "C" fn msg_cb(write_p: c_int, version: c_int, content_type: c_int, buf: *const c_void, len: usize, ssl: *mut c_void, _arg: *mut c_void) {
    unsafe {
        let g = capi::PyGILState_Ensure();
        let sock = sl!(SSL_get_ex_data(ssl, SSL_EX_INDEX)) as *mut PyObject;
        if !sock.is_null() {
            let s = ss(&*sock);
            if let Some(cb) = ctx(&s.context).msg_callback.clone() {
                let data = std::slice::from_raw_parts(buf as *const u8, len);
                let msg_type = match content_type {
                    20 => 0x101,
                    21 if data.len() >= 2 => data[1] as i64,
                    _ if !data.is_empty() => data[0] as i64,
                    _ => -1,
                };
                let owner = owner_or_self(&*sock);
                let direction = new_str(if write_p != 0 { "write" } else { "read" });
                let r = call(
                    &cb,
                    &[&owner, &direction, &new_int(version as i64), &new_int(content_type as i64), &new_int(msg_type), &bytes::new_bytes(data)],
                );
                if r.is_err() {
                    if let Some(e) = rpy_runtime::errors::take_exc() {
                        if s.pending_exc.is_none() {
                            s.pending_exc = Some(e);
                        }
                    }
                }
            }
        }
        capi::PyGILState_Release(g);
    }
}

pub fn init_type() -> PyResult<()> {
    unsafe {
        let t = socket_type();
        fc::init_robj_type::<SslSock>(t, c"_ssl._SSLSocket", Py_TPFLAGS_DISALLOW_INSTANTIATION);
        (*t).tp_methods = rpy_runtime::mdefs![
            ("do_handshake", NOARGS, m_do_handshake),
            ("read", FAST, m_read),
            ("write", O, m_write),
            ("pending", NOARGS, m_pending),
            ("getpeercert", FAST, m_getpeercert),
            ("get_verified_chain", NOARGS, m_get_verified_chain),
            ("get_unverified_chain", NOARGS, m_get_unverified_chain),
            ("cipher", NOARGS, m_cipher),
            ("shared_ciphers", NOARGS, m_shared_ciphers),
            ("version", NOARGS, m_version),
            ("selected_alpn_protocol", NOARGS, m_selected_alpn),
            ("compression", NOARGS, m_compression),
            ("get_channel_binding", FASTKW, m_get_channel_binding),
            ("verify_client_post_handshake", NOARGS, m_verify_client_post_handshake),
            ("shutdown", NOARGS, m_shutdown),
        ];
        (*t).tp_getset = rpy_runtime::getset_table![
            ("context", g_context, s_context),
            ("server_side", g_server_side),
            ("server_hostname", g_server_hostname),
            ("owner", g_owner, s_owner),
            ("session", g_session, set_session),
            ("session_reused", g_session_reused),
            ("uses_ktls_for_send", g_false),
            ("uses_ktls_for_recv", g_false),
        ];
        rpy_runtime::types::typeobject::type_ready(t)?;
    }
    Ok(())
}
