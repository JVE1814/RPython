//! OpenSSL 3 (libssl.so.3 / libcrypto.so.3) entry points, loaded with dlopen.
//! Declarations written from the documented OpenSSL API.
#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_void};

pub type P = *mut c_void;

crate::dyn_lib!(LibCrypto, ["libcrypto.so.3", "libcrypto.so"], {
    fn OpenSSL_version(t: c_int) -> *const c_char;
    fn OpenSSL_version_num() -> c_ulong;
    fn ERR_get_error() -> c_ulong;
    fn ERR_peek_last_error() -> c_ulong;
    fn ERR_clear_error();
    fn ERR_reason_error_string(e: c_ulong) -> *const c_char;
    fn ERR_lib_error_string(e: c_ulong) -> *const c_char;
    fn ERR_error_string_n(e: c_ulong, buf: *mut c_char, n: usize);
    fn RAND_bytes(buf: *mut c_uchar, n: c_int) -> c_int;
    fn RAND_status() -> c_int;
    fn RAND_add(buf: *const c_void, n: c_int, entropy: f64);
    fn BIO_new(m: P) -> P;
    fn BIO_s_mem() -> P;
    fn BIO_free(b: P) -> c_int;
    fn BIO_free_all(b: P);
    fn BIO_up_ref(b: P) -> c_int;
    fn BIO_read(b: P, buf: *mut c_void, n: c_int) -> c_int;
    fn BIO_write(b: P, buf: *const c_void, n: c_int) -> c_int;
    fn BIO_ctrl(b: P, cmd: c_int, larg: c_long, parg: P) -> c_long;
    fn BIO_ctrl_pending(b: P) -> usize;
    fn BIO_new_file(name: *const c_char, mode: *const c_char) -> P;
    fn BIO_new_mem_buf(buf: *const c_void, n: c_int) -> P;
    fn BIO_test_flags(b: P, flags: c_int) -> c_int;
    fn BIO_clear_flags(b: P, flags: c_int);
    fn X509_free(x: P);
    fn X509_up_ref(x: P) -> c_int;
    fn i2d_X509(x: P, out: *mut *mut c_uchar) -> c_int;
    fn d2i_X509(x: *mut P, inp: *mut *const c_uchar, n: c_long) -> P;
    fn PEM_read_bio_X509(b: P, x: *mut P, cb: P, u: P) -> P;
    fn PEM_read_bio_X509_AUX(b: P, x: *mut P, cb: P, u: P) -> P;
    fn PEM_write_bio_X509(b: P, x: P) -> c_int;
    fn PEM_read_bio_Parameters(b: P, x: *mut P) -> P;
    fn PEM_read_bio_DHparams(b: P, x: *mut P, cb: P, u: P) -> P;
    fn DH_free(d: P);
    fn EVP_PKEY_free(k: P);
    fn X509_get_subject_name(x: P) -> P;
    fn X509_get_issuer_name(x: P) -> P;
    fn X509_get_version(x: P) -> c_long;
    fn X509_get_serialNumber(x: P) -> P;
    fn X509_get0_notBefore(x: P) -> P;
    fn X509_get0_notAfter(x: P) -> P;
    fn X509_get_ext_d2i(x: P, nid: c_int, crit: *mut c_int, idx: *mut c_int) -> P;
    fn X509_check_ca(x: P) -> c_int;
    fn X509_NAME_entry_count(n: P) -> c_int;
    fn X509_NAME_get_entry(n: P, i: c_int) -> P;
    fn X509_NAME_ENTRY_set(e: P) -> c_int;
    fn X509_NAME_ENTRY_get_object(e: P) -> P;
    fn X509_NAME_ENTRY_get_data(e: P) -> P;
    fn X509_NAME_oneline(n: P, buf: *mut c_char, size: c_int) -> *mut c_char;
    fn ASN1_STRING_to_UTF8(out: *mut *mut c_uchar, s: P) -> c_int;
    fn ASN1_STRING_length(s: P) -> c_int;
    fn ASN1_STRING_get0_data(s: P) -> *const c_uchar;
    fn ASN1_STRING_type(s: P) -> c_int;
    fn ASN1_TIME_print(b: P, t: P) -> c_int;
    fn ASN1_INTEGER_to_BN(i: P, bn: P) -> P;
    fn BN_bn2hex(bn: P) -> *mut c_char;
    fn BN_free(bn: P);
    fn CRYPTO_free(p: *mut c_void, file: *const c_char, line: c_int);
    fn OBJ_obj2nid(o: P) -> c_int;
    fn OBJ_nid2sn(n: c_int) -> *const c_char;
    fn OBJ_nid2ln(n: c_int) -> *const c_char;
    fn OBJ_nid2obj(n: c_int) -> P;
    fn OBJ_txt2obj(s: *const c_char, no_name: c_int) -> P;
    fn OBJ_sn2nid(s: *const c_char) -> c_int;
    fn OBJ_ln2nid(s: *const c_char) -> c_int;
    fn OBJ_obj2txt(buf: *mut c_char, n: c_int, o: P, no_name: c_int) -> c_int;
    fn ASN1_OBJECT_free(o: P);
    fn OPENSSL_sk_num(s: P) -> c_int;
    fn OPENSSL_sk_value(s: P, i: c_int) -> P;
    fn OPENSSL_sk_free(s: P);
    fn OPENSSL_sk_pop_free(s: P, f: Option<unsafe extern "C" fn(P)>);
    fn GENERAL_NAME_free(g: P);
    fn GENERAL_NAMES_free(g: P);
    fn AUTHORITY_INFO_ACCESS_free(a: P);
    fn CRL_DIST_POINTS_free(c: P);
    fn X509_STORE_add_cert(st: P, x: P) -> c_int;
    fn X509_STORE_add_crl(st: P, x: P) -> c_int;
    fn X509_STORE_get0_objects(st: P) -> P;
    fn X509_STORE_get0_param(st: P) -> P;
    fn X509_OBJECT_get_type(o: P) -> c_int;
    fn X509_OBJECT_get0_X509(o: P) -> P;
    fn X509_VERIFY_PARAM_set_flags(p: P, f: c_ulong) -> c_int;
    fn X509_VERIFY_PARAM_clear_flags(p: P, f: c_ulong) -> c_int;
    fn X509_VERIFY_PARAM_get_flags(p: P) -> c_ulong;
    fn X509_VERIFY_PARAM_set_hostflags(p: P, f: c_uint);
    fn X509_VERIFY_PARAM_get_hostflags(p: P) -> c_uint;
    fn X509_VERIFY_PARAM_set1_host(p: P, name: *const c_char, n: usize) -> c_int;
    fn X509_VERIFY_PARAM_set1_ip(p: P, ip: *const c_uchar, n: usize) -> c_int;
    fn X509_verify_cert_error_string(n: c_long) -> *const c_char;
    fn X509_get_default_cert_file_env() -> *const c_char;
    fn X509_get_default_cert_file() -> *const c_char;
    fn X509_get_default_cert_dir_env() -> *const c_char;
    fn X509_get_default_cert_dir() -> *const c_char;
    fn d2i_X509_CRL(x: *mut P, inp: *mut *const c_uchar, n: c_long) -> P;
    fn PEM_read_bio_X509_CRL(b: P, x: *mut P, cb: P, u: P) -> P;
    fn X509_CRL_free(c: P);
    fn EVP_get_digestbyname(n: *const c_char) -> P;
    fn X509_digest(x: P, md: P, out: *mut c_uchar, n: *mut c_uint) -> c_int;
    fn X509_get_signature_nid(x: P) -> c_int;
    fn OBJ_find_sigid_algs(sig: c_int, md: *mut c_int, pk: *mut c_int) -> c_int;
    fn EVP_MD_get_size(md: P) -> c_int;
    // (tail2) distrusted-key check in the verify callback
    fn X509_STORE_CTX_get_current_cert(c: P) -> P;
    fn X509_STORE_CTX_set_error(c: P, e: c_int);
    fn X509_get_X509_PUBKEY(x: P) -> P;
    fn i2d_X509_PUBKEY(k: P, out: *mut *mut c_uchar) -> c_int;
    fn EVP_Digest(d: *const c_void, n: usize, md: *mut c_uchar, len: *mut c_uint, t: P, e: P) -> c_int;
    fn EVP_sha256() -> P;
});

crate::dyn_lib!(LibSsl, ["libssl.so.3", "libssl.so"], {
    fn TLS_method() -> P;
    fn TLS_client_method() -> P;
    fn TLS_server_method() -> P;
    fn TLSv1_method() -> P;
    fn TLSv1_1_method() -> P;
    fn TLSv1_2_method() -> P;
    fn SSL_CTX_new(m: P) -> P;
    fn SSL_CTX_free(c: P);
    fn SSL_CTX_up_ref(c: P) -> c_int;
    fn SSL_CTX_set_options(c: P, o: u64) -> u64;
    fn SSL_CTX_clear_options(c: P, o: u64) -> u64;
    fn SSL_CTX_get_options(c: P) -> u64;
    fn SSL_CTX_ctrl(c: P, cmd: c_int, l: c_long, p: P) -> c_long;
    fn SSL_CTX_callback_ctrl(c: P, cmd: c_int, f: P) -> c_long;
    fn SSL_CTX_set_verify(c: P, mode: c_int, cb: P);
    fn SSL_CTX_get_verify_mode(c: P) -> c_int;
    fn SSL_CTX_set_cipher_list(c: P, s: *const c_char) -> c_int;
    fn SSL_CTX_set_ciphersuites(c: P, s: *const c_char) -> c_int;
    fn SSL_CTX_get_ciphers(c: P) -> P;
    fn SSL_CTX_use_certificate_chain_file(c: P, f: *const c_char) -> c_int;
    fn SSL_CTX_use_PrivateKey_file(c: P, f: *const c_char, t: c_int) -> c_int;
    fn SSL_CTX_check_private_key(c: P) -> c_int;
    fn SSL_CTX_set_default_passwd_cb(c: P, cb: P);
    fn SSL_CTX_set_default_passwd_cb_userdata(c: P, u: P);
    fn SSL_CTX_load_verify_locations(c: P, f: *const c_char, d: *const c_char) -> c_int;
    fn SSL_CTX_set_default_verify_paths(c: P) -> c_int;
    fn SSL_CTX_get_cert_store(c: P) -> P;
    fn SSL_CTX_set_alpn_protos(c: P, p: *const c_uchar, n: c_uint) -> c_int;
    fn SSL_CTX_set_alpn_select_cb(c: P, cb: P, arg: P);
    fn SSL_CTX_set_keylog_callback(c: P, cb: P);
    fn SSL_CTX_set_post_handshake_auth(c: P, v: c_int);
    fn SSL_CTX_set_security_level(c: P, l: c_int);
    fn SSL_CTX_get_security_level(c: P) -> c_int;
    fn SSL_CTX_get0_param(c: P) -> P;
    fn SSL_CTX_set_num_tickets(c: P, n: usize) -> c_int;
    fn SSL_CTX_get_num_tickets(c: P) -> usize;
    fn SSL_CTX_set_session_id_context(c: P, s: *const c_uchar, n: c_uint) -> c_int;
    fn SSL_CTX_set0_tmp_dh_pkey(c: P, k: P) -> c_int;
    fn SSL_CTX_set_msg_callback(c: P, cb: P);
    fn SSL_CTX_set_ex_data(c: P, i: c_int, d: P) -> c_int;
    fn SSL_CTX_get_ex_data(c: P, i: c_int) -> P;
    fn SSL_new(c: P) -> P;
    fn SSL_free(s: P);
    fn SSL_set_fd(s: P, fd: c_int) -> c_int;
    fn SSL_set_bio(s: P, r: P, w: P);
    fn SSL_set_connect_state(s: P);
    fn SSL_set_accept_state(s: P);
    fn SSL_do_handshake(s: P) -> c_int;
    fn SSL_read_ex(s: P, buf: *mut c_void, n: usize, read: *mut usize) -> c_int;
    fn SSL_write_ex(s: P, buf: *const c_void, n: usize, written: *mut usize) -> c_int;
    fn SSL_pending(s: P) -> c_int;
    fn SSL_shutdown(s: P) -> c_int;
    fn SSL_get_shutdown(s: P) -> c_int;
    fn SSL_set_shutdown(s: P, m: c_int);
    fn SSL_get_error(s: P, r: c_int) -> c_int;
    fn SSL_ctrl(s: P, cmd: c_int, l: c_long, p: P) -> c_long;
    fn SSL_get1_peer_certificate(s: P) -> P;
    fn SSL_get_peer_cert_chain(s: P) -> P;
    fn SSL_get0_verified_chain(s: P) -> P;
    fn SSL_get_current_cipher(s: P) -> P;
    fn SSL_get_ciphers(s: P) -> P;
    fn SSL_get_client_ciphers(s: P) -> P;
    fn SSL_get_version(s: P) -> *const c_char;
    fn SSL_version(s: P) -> c_int;
    fn SSL_get0_alpn_selected(s: P, d: *mut *const c_uchar, n: *mut c_uint);
    fn SSL_get_finished(s: P, buf: *mut c_void, n: usize) -> usize;
    fn SSL_get_peer_finished(s: P, buf: *mut c_void, n: usize) -> usize;
    fn SSL_session_reused(s: P) -> c_int;
    fn SSL_get1_session(s: P) -> P;
    fn SSL_SESSION_dup(s: P) -> P;
    fn SSL_set_session(s: P, sess: P) -> c_int;
    fn SSL_get_servername(s: P, t: c_int) -> *const c_char;
    fn SSL_set_SSL_CTX(s: P, c: P) -> P;
    fn SSL_get_SSL_CTX(s: P) -> P;
    fn SSL_verify_client_post_handshake(s: P) -> c_int;
    fn SSL_get_verify_result(s: P) -> c_long;
    fn SSL_set_ex_data(s: P, i: c_int, d: P) -> c_int;
    fn SSL_get_ex_data(s: P, i: c_int) -> P;
    fn SSL_get_ex_data_X509_STORE_CTX_idx() -> c_int;
    fn SSL_get0_param(s: P) -> P;
    fn SSL_is_init_finished(s: P) -> c_int;
    fn SSL_in_init(s: P) -> c_int;
    fn SSL_get_rbio(s: P) -> P;
    fn SSL_set_post_handshake_auth(s: P, v: c_int);
    fn SSL_set_verify(s: P, mode: c_int, cb: P);
    fn SSL_get_verify_mode(s: P) -> c_int;
    fn SSL_CIPHER_get_name(c: P) -> *const c_char;
    fn SSL_CIPHER_get_version(c: P) -> *const c_char;
    fn SSL_CIPHER_get_bits(c: P, alg: *mut c_int) -> c_int;
    fn SSL_CIPHER_description(c: P, buf: *mut c_char, n: c_int) -> *mut c_char;
    fn SSL_CIPHER_get_id(c: P) -> u32;
    fn SSL_CIPHER_get_protocol_id(c: P) -> u16;
    fn SSL_CIPHER_is_aead(c: P) -> c_int;
    fn SSL_CIPHER_get_cipher_nid(c: P) -> c_int;
    fn SSL_CIPHER_get_digest_nid(c: P) -> c_int;
    fn SSL_CIPHER_get_kx_nid(c: P) -> c_int;
    fn SSL_CIPHER_get_auth_nid(c: P) -> c_int;
    fn SSL_SESSION_free(s: P);
    fn SSL_SESSION_up_ref(s: P) -> c_int;
    fn SSL_SESSION_get_id(s: P, n: *mut c_uint) -> *const c_uchar;
    fn SSL_SESSION_get_time(s: P) -> c_long;
    fn SSL_SESSION_get_timeout(s: P) -> c_long;
    fn SSL_SESSION_has_ticket(s: P) -> c_int;
    fn SSL_SESSION_get_ticket_lifetime_hint(s: P) -> c_ulong;
    fn SSL_SESSION_is_resumable(s: P) -> c_int;
    fn SSL_select_next_proto(out: *mut *mut c_uchar, outlen: *mut c_uchar, srv: *const c_uchar, srvlen: c_uint, cli: *const c_uchar, clilen: c_uint) -> c_int;
    fn SSL_group_to_name(s: P, id: c_int) -> *const c_char;
});

pub fn crypto() -> &'static LibCrypto {
    LibCrypto::get().expect("libcrypto not available")
}
pub fn ssl() -> &'static LibSsl {
    LibSsl::get().expect("libssl not available")
}

#[macro_export]
macro_rules! cr {
    ($f:ident ( $($a:expr),* $(,)? )) => {
        unsafe { ($crate::ssl::ffi::crypto().$f.expect(stringify!($f)))($($a),*) }
    };
}
#[macro_export]
macro_rules! sl {
    ($f:ident ( $($a:expr),* $(,)? )) => {
        unsafe { ($crate::ssl::ffi::ssl().$f.expect(stringify!($f)))($($a),*) }
    };
}

pub const SSL_ERROR_NONE: c_int = 0;
pub const SSL_ERROR_SSL: c_int = 1;
pub const SSL_ERROR_WANT_READ: c_int = 2;
pub const SSL_ERROR_WANT_WRITE: c_int = 3;
pub const SSL_ERROR_WANT_X509_LOOKUP: c_int = 4;
pub const SSL_ERROR_SYSCALL: c_int = 5;
pub const SSL_ERROR_ZERO_RETURN: c_int = 6;
pub const SSL_ERROR_WANT_CONNECT: c_int = 7;

pub const BIO_CTRL_EOF: c_int = 2;
pub const BIO_CTRL_PENDING: c_int = 10;
pub const BIO_C_SET_BUF_MEM_EOF_RETURN: c_int = 130;
pub const BIO_FLAGS_SHOULD_RETRY: c_int = 0x08;

pub const SSL_CTRL_SET_TMP_DH: c_int = 3;
pub const SSL_CTRL_MODE: c_int = 33;
pub const SSL_CTRL_SET_SESS_CACHE_MODE: c_int = 44;
pub const SSL_CTRL_SET_TLSEXT_SERVERNAME_CB: c_int = 53;
pub const SSL_CTRL_SET_TLSEXT_HOSTNAME: c_int = 55;
pub const SSL_CTRL_SET_GROUPS_LIST: c_int = 92;
pub const SSL_CTRL_SET_MIN_PROTO_VERSION: c_int = 123;
pub const SSL_CTRL_SET_MAX_PROTO_VERSION: c_int = 124;
pub const SSL_CTRL_GET_MIN_PROTO_VERSION: c_int = 130;
pub const SSL_CTRL_GET_MAX_PROTO_VERSION: c_int = 131;

pub const SSL_MODE_ENABLE_PARTIAL_WRITE: c_long = 1;
pub const SSL_MODE_ACCEPT_MOVING_WRITE_BUFFER: c_long = 2;
pub const SSL_MODE_AUTO_RETRY: c_long = 4;

pub const SSL_VERIFY_NONE: c_int = 0;
pub const SSL_VERIFY_PEER: c_int = 1;
pub const SSL_VERIFY_FAIL_IF_NO_PEER_CERT: c_int = 2;
pub const SSL_VERIFY_POST_HANDSHAKE: c_int = 8;

pub const SSL_RECEIVED_SHUTDOWN: c_int = 2;
pub const SSL_SENT_SHUTDOWN: c_int = 1;

pub const NID_SUBJECT_ALT_NAME: c_int = 85;
pub const NID_INFO_ACCESS: c_int = 177;
pub const NID_CRL_DISTRIBUTION_POINTS: c_int = 103;
pub const NID_AD_OCSP: c_int = 178;
pub const NID_AD_CA_ISSUERS: c_int = 179;
