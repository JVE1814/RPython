//! Hash modules: `_md5`, `_sha1`, `_sha2`, `_sha3`, `_blake2` (the "builtin" implementations),
//! `_hashlib` (an OpenSSL-API-compatible module backed by the same Rust code: `new`,
//! `openssl_*`, HASH/HASHXOF/HMAC, `pbkdf2_hmac`, `scrypt`, `hmac_digest`, ...) and `_hmac`
//! (the builtin HMAC module of CPython 3.14+).

use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::modules::compress_common::{self as cc, ExcCell};
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, int, str as pystr, tuple};
use rpy_compress::hash::{self as h, Blake2Params, Blake2b, Blake2s, Digest};

pub const GIL_MINSIZE: i64 = 2048;
const MAX_XOF: isize = 1 << 29;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    Sha3,
    Shake,
    Blake,
}

pub struct HState {
    h: Box<dyn Digest>,
    name: &'static str,
    kind: Kind,
}

pub struct HmacState {
    inner: Box<dyn Digest>,
    outer: Box<dyn Digest>,
    name: &'static str,
}

// Type indices.
const T_MD5: usize = 0;
const T_SHA1: usize = 1;
const T_SHA224: usize = 2;
const T_SHA256: usize = 3;
const T_SHA384: usize = 4;
const T_SHA512: usize = 5;
const T_SHA3_224: usize = 6;
const T_SHAKE128: usize = 10;
const T_SHAKE256: usize = 11;
const T_BLAKE2B: usize = 12;
const T_BLAKE2S: usize = 13;
const T_HASH: usize = 14;
const T_HASHXOF: usize = 15;
const T_OHMAC: usize = 16;
const T_BHMAC: usize = 17;
const NTYPES: usize = 18;

static mut TYPES: [PyTypeObject; NTYPES] = [const { PyTypeObject::ZERO }; NTYPES];

fn tp(i: usize) -> *mut PyTypeObject {
    unsafe { (&raw mut TYPES).cast::<PyTypeObject>().add(i) }
}

static UNSUPPORTED: ExcCell = ExcCell::new();
static UNKNOWN_HASH: ExcCell = ExcCell::new();

/// Canonical algorithm names known to this build.
const ALGOS: &[&str] = &[
    "md5", "sha1", "sha224", "sha256", "sha384", "sha512", "sha512_224", "sha512_256", "sha3_224", "sha3_256", "sha3_384", "sha3_512", "shake_128",
    "shake_256", "blake2b", "blake2s",
];

fn canonical(name: &str) -> Option<&'static str> {
    let l = name.to_ascii_lowercase();
    let l = match l.as_str() {
        "sha-1" => "sha1".to_string(),
        "sha-224" => "sha224".into(),
        "sha-256" => "sha256".into(),
        "sha-384" => "sha384".into(),
        "sha-512" => "sha512".into(),
        "sha512-224" | "sha-512/224" | "sha512/224" => "sha512_224".into(),
        "sha512-256" | "sha-512/256" | "sha512/256" => "sha512_256".into(),
        "sha3-224" => "sha3_224".into(),
        "sha3-256" => "sha3_256".into(),
        "sha3-384" => "sha3_384".into(),
        "sha3-512" => "sha3_512".into(),
        "shake128" => "shake_128".into(),
        "shake256" => "shake_256".into(),
        "blake2b512" => "blake2b".into(),
        "blake2s256" => "blake2s".into(),
        _ => l,
    };
    ALGOS.iter().copied().find(|a| *a == l)
}

fn make_digest(name: &str) -> Option<(Box<dyn Digest>, Kind)> {
    Some(match name {
        "shake_128" => (Box::new(h::Sha3::shake(128)), Kind::Shake),
        "shake_256" => (Box::new(h::Sha3::shake(256)), Kind::Shake),
        n if n.starts_with("sha3_") => (h::by_name(n)?, Kind::Sha3),
        n if n.starts_with("blake2") => (h::by_name(n)?, Kind::Blake),
        n => (h::by_name(n)?, Kind::Plain),
    })
}

fn is_str(o: &PyObject) -> bool {
    pystr::is_str(o)
}

/// Bytes-like argument of a hash function ("Strings must be encoded before hashing").
fn hash_data(o: &PyObject) -> PyResult<crate::modules::_struct::Buf> {
    if is_str(o) {
        raise!(TypeError, "Strings must be encoded before hashing");
    }
    match cc::buffer(o) {
        Ok(b) => Ok(b),
        Err(e) => {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                raise!(TypeError, "object supporting the buffer API required");
            }
            Err(e)
        }
    }
}

fn update_digest(d: &mut dyn Digest, data: &[u8]) {
    if data.len() as i64 >= GIL_MINSIZE * 32 {
        crate::threads::blocking(|| d.update(data));
    } else {
        d.update(data);
    }
}

// ------------------------------------------------------------------ constructor arguments

/// Parse `(<pos names>..., *, <kw names>..., usedforsecurity=True, string=None)` with
/// CPython 3.15's checks: duplicated arguments, then unknown keywords, then the
/// 'data'/'string' conflict.  Returns values for `names` followed by `string`.
fn parse_ctor<'b>(fname: &str, npos: usize, names: &[&str], a: &[&'b PyObject], kw: Option<&'b PyObject>) -> PyResult<Vec<Option<&'b PyObject>>> {
    let (pos, kws) = args::split(a, kw);
    let mut all: Vec<&str> = names.to_vec();
    all.push("usedforsecurity");
    all.push("string");
    let mut out: Vec<Option<&'b PyObject>> = vec![None; all.len()];
    if pos.len() > npos {
        if npos == 1 {
            raise!(TypeError, "{}() takes at most 1 positional argument ({} given)", fname, pos.len());
        }
        raise!(TypeError, "{}() takes at most {} positional arguments ({} given)", fname, npos, pos.len());
    }
    for (i, p) in pos.iter().enumerate() {
        out[i] = Some(p);
    }
    let kstr: Vec<(String, &'b PyObject)> = kws.iter().map(|(k, v)| (pystr::to_string_lossy(k), *v)).collect();
    for (k, _) in &kstr {
        if let Some(i) = all.iter().position(|n| n == k) {
            if i < pos.len() {
                raise!(TypeError, "argument for {}() given by name ('{}') and position ({})", fname, k, i + 1);
            }
        }
    }
    for (k, v) in &kstr {
        match all.iter().position(|n| n == k) {
            Some(i) => out[i] = Some(v),
            None => raise!(TypeError, "{}() got an unexpected keyword argument '{}'", fname, k),
        }
    }
    let di = names.iter().position(|n| *n == "data");
    let si = all.len() - 1;
    if let (Some(di), Some(_)) = (di, out[si]) {
        if out[di].is_some() {
            raise!(
                TypeError,
                "'data' and 'string' are mutually exclusive and support for 'string' keyword parameter is slated for removal in a future version."
            );
        }
        crate::modules::warnings::warn(
            crate::types::exceptions::DeprecationWarning(),
            "the 'string' keyword parameter is deprecated since Python 3.15 and slated for removal in Python 3.19; use the 'data' keyword parameter or pass the data to hash as a positional argument instead",
            1,
        )?;
        out[di] = out[si];
    }
    Ok(out)
}

fn new_hash_obj(t: usize, d: Box<dyn Digest>, name: &'static str, kind: Kind, data: Option<&PyObject>) -> PyResult {
    let mut st = HState { h: d, name, kind };
    if let Some(o) = data {
        let b = hash_data(o)?;
        update_digest(&mut *st.h, b.data());
    }
    cc::new_obj(tp(t), st)
}

fn builtin_type_for(name: &str) -> usize {
    match name {
        "md5" => T_MD5,
        "sha1" => T_SHA1,
        "sha224" => T_SHA224,
        "sha256" => T_SHA256,
        "sha384" => T_SHA384,
        "sha512" => T_SHA512,
        "sha3_224" => T_SHA3_224,
        "sha3_256" => T_SHA3_224 + 1,
        "sha3_384" => T_SHA3_224 + 2,
        "sha3_512" => T_SHA3_224 + 3,
        "shake_128" => T_SHAKE128,
        "shake_256" => T_SHAKE256,
        "blake2b" => T_BLAKE2B,
        _ => T_BLAKE2S,
    }
}

/// Builtin module-level constructor (`_md5.md5`, `_sha2.sha256`, ...): the function name is
/// the algorithm name.
fn builtin_ctor(name: &'static str, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let v = parse_ctor(name, 1, &["data"], a, kw)?;
    let (d, kind) = make_digest(name).unwrap();
    new_hash_obj(builtin_type_for(name), d, name, kind, v[0])
}

macro_rules! ctor_fn {
    ($f:ident, $n:expr) => {
        fn $f(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
            builtin_ctor($n, a, kw)
        }
    };
}
ctor_fn!(c_md5, "md5");
ctor_fn!(c_sha1, "sha1");
ctor_fn!(c_sha224, "sha224");
ctor_fn!(c_sha256, "sha256");
ctor_fn!(c_sha384, "sha384");
ctor_fn!(c_sha512, "sha512");

/// tp_new for the instantiable `_sha3` types.
unsafe extern "C" fn sha3_new(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let mut ti = 0;
            for i in T_SHA3_224..=T_SHAKE256 {
                if is_subtype(t, tp(i)) {
                    ti = i;
                }
            }
            let name = ["sha3_224", "sha3_256", "sha3_384", "sha3_512", "shake_128", "shake_256"][ti - T_SHA3_224];
            let (v, kn) = args::kwdict_to_vectorcall(&*a, kw);
            let vr: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            let p = parse_ctor(name, 1, &["data"], &vr, kn.as_deref())?;
            let (d, kind) = make_digest(name).unwrap();
            let mut st = HState { h: d, name, kind };
            if let Some(o) = p[0] {
                let b = hash_data(o)?;
                update_digest(&mut *st.h, b.data());
            }
            cc::new_obj(t, st)
        })();
        into_c(r)
    }
}

fn is_subtype(a: *mut PyTypeObject, b: *mut PyTypeObject) -> bool {
    crate::types::typeobject::is_subtype(a, b)
}

/// tp_new for `_blake2.blake2b` / `_blake2.blake2s`.
unsafe extern "C" fn blake2_new(t: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let big = is_subtype(t, tp(T_BLAKE2B));
            let name: &'static str = if big { "blake2b" } else { "blake2s" };
            let (v, kn) = args::kwdict_to_vectorcall(&*a, kw);
            let vr: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            let names = [
                "data", "digest_size", "key", "salt", "person", "fanout", "depth", "leaf_size", "node_offset", "node_depth", "inner_size", "last_node",
            ];
            let p = parse_ctor(name, 1, &names, &vr, kn.as_deref())?;
            let (maxd, maxk, salt_n, pers_n, max_off): (i64, usize, usize, usize, u64) =
                if big { (64, 64, 16, 16, u64::MAX) } else { (32, 32, 8, 8, (1u64 << 48) - 1) };
            let mut prm = Blake2Params { digest_size: maxd as usize, fanout: 1, depth: 1, ..Default::default() };
            if let Some(o) = p[1] {
                let v = cc::c_int(o)? as i64;
                if v < 1 || v > maxd {
                    raise!(ValueError, "digest_size for {} must be between 1 and {} bytes, here it is {}", name, maxd, v);
                }
                prm.digest_size = v as usize;
            }
            let bytes_arg = |o: Option<&PyObject>| -> PyResult<Vec<u8>> {
                match o {
                    None => Ok(Vec::new()),
                    Some(o) => Ok(cc::buffer(o)?.data().to_vec()),
                }
            };
            prm.key = bytes_arg(p[2])?;
            if prm.key.len() > maxk {
                raise!(ValueError, "maximum key length is {} bytes", maxk);
            }
            prm.salt = bytes_arg(p[3])?;
            if prm.salt.len() > salt_n {
                raise!(ValueError, "maximum salt length is {} bytes", salt_n);
            }
            prm.person = bytes_arg(p[4])?;
            if prm.person.len() > pers_n {
                raise!(ValueError, "maximum person length is {} bytes", pers_n);
            }
            let small = |o: Option<&PyObject>, def: i32, lo: i32, what: &str| -> PyResult<u8> {
                match o {
                    None => Ok(def as u8),
                    Some(o) => {
                        let v = cc::c_int(o)?;
                        if v < lo || v > 255 {
                            if what == "depth" {
                                raise!(ValueError, "depth must be between 1 and 255");
                            }
                            raise!(ValueError, "{} must be between 0 and 255", what);
                        }
                        Ok(v as u8)
                    }
                }
            };
            prm.fanout = small(p[5], 1, 0, "fanout")?;
            prm.depth = small(p[6], 1, 1, "depth")?;
            if let Some(o) = p[7] {
                let i = ab::index(o)?;
                if int::sign(&i) < 0 {
                    raise!(ValueError, "leaf_size must be positive");
                }
                match int::as_i64(&i) {
                    Some(v) if v <= u32::MAX as i64 => prm.leaf_size = v as u32,
                    _ => raise!(OverflowError, "leaf_size is too large"),
                }
            }
            if let Some(o) = p[8] {
                let i = ab::index(o)?;
                if int::sign(&i) < 0 {
                    raise!(ValueError, "node_offset must be positive");
                }
                match rpy_bigint::conv::to_u64(crate::types::int_methods::iref(&i)) {
                    Some(v) if v <= max_off => prm.node_offset = v,
                    _ => raise!(OverflowError, "node_offset is too large"),
                }
            }
            prm.node_depth = small(p[9], 0, 0, "node_depth")?;
            if let Some(o) = p[10] {
                let v = cc::c_int(o)?;
                if v < 0 || v as i64 > maxd {
                    raise!(ValueError, "inner_size must be between 0 and is {}", maxd);
                }
                prm.inner_size = v as u8;
            }
            if let Some(o) = p[11] {
                prm.last_node = ab::is_true(o)?;
            }
            let d: Box<dyn Digest> = if big { Box::new(Blake2b::new(&prm)) } else { Box::new(Blake2s::new(&prm)) };
            let mut st = HState { h: d, name, kind: Kind::Blake };
            if let Some(o) = p[0] {
                let b = hash_data(o)?;
                update_digest(&mut *st.h, b.data());
            }
            cc::new_obj(t, st)
        })();
        into_c(r)
    }
}

// ------------------------------------------------------------------ hash object methods

fn h_update(o: &PyObject, data: &PyObject) -> PyResult {
    let b = hash_data(data)?;
    let mut st = cc::lock::<HState>(o)?;
    update_digest(&mut *st.h, b.data());
    Ok(cc::none())
}

fn h_digest(o: &PyObject) -> PyResult {
    let st = cc::lock::<HState>(o)?;
    let d = st.h.digest();
    drop(st);
    cc::new_bytes(&d)
}

fn h_hexdigest(o: &PyObject) -> PyResult {
    let st = cc::lock::<HState>(o)?;
    let d = st.h.digest();
    Ok(pystr::new_str(&h::hex(&d)))
}

fn xof_len(a: &[&PyObject], kw: Option<&PyObject>, fname: &str) -> PyResult<usize> {
    static P: Params = Params::new("digest", &["length"], 0, 1);
    let _ = fname;
    let [l] = args::parse::<1>(&P, a, kw)?;
    let n = cc::ssize(l.unwrap())?;
    if n < 0 {
        raise!(ValueError, "negative digest length");
    }
    if n >= MAX_XOF {
        raise!(ValueError, "digest length is too large");
    }
    Ok(n as usize)
}

fn x_digest(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let n = xof_len(a, kw, "digest")?;
    let st = cc::lock::<HState>(o)?;
    let d = st.h.xof(n).unwrap_or_default();
    drop(st);
    cc::new_bytes(&d)
}

fn x_hexdigest(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let n = xof_len(a, kw, "hexdigest")?;
    let st = cc::lock::<HState>(o)?;
    let d = st.h.xof(n).unwrap_or_default();
    Ok(pystr::new_str(&h::hex(&d)))
}

fn h_copy(o: &PyObject) -> PyResult {
    let st = cc::lock::<HState>(o)?;
    let n = HState { h: st.h.box_clone(), name: st.name, kind: st.kind };
    drop(st);
    cc::new_obj(o.type_ptr(), n)
}

fn h_name(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(cc::lock::<HState>(o)?.name))
}
fn h_digest_size(o: &PyObject) -> PyResult {
    let st = cc::lock::<HState>(o)?;
    Ok(int::new_int(if st.kind == Kind::Shake { 0 } else { st.h.digest_size() as i64 }))
}
fn h_block_size(o: &PyObject) -> PyResult {
    Ok(int::new_int(cc::lock::<HState>(o)?.h.block_size() as i64))
}
fn h_rate_bits(o: &PyObject) -> PyResult {
    Ok(int::new_int(cc::lock::<HState>(o)?.h.block_size() as i64 * 8))
}
fn h_capacity_bits(o: &PyObject) -> PyResult {
    Ok(int::new_int(1600 - cc::lock::<HState>(o)?.h.block_size() as i64 * 8))
}
fn h_suffix(o: &PyObject) -> PyResult {
    let st = cc::lock::<HState>(o)?;
    cc::new_bytes(if st.kind == Kind::Shake { b"\x1f" } else { b"\x06" })
}

fn h_repr(o: &PyObject) -> PyResult {
    let st = cc::lock::<HState>(o)?;
    Ok(pystr::new_str(&format!("<{} {} object @ {:#x}>", st.name, crate::types::typeobject::type_name(o.ty()), o.as_ptr() as usize)))
}

unsafe extern "C" fn h_repr_slot(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(h_repr(&*o)) }
}

// ------------------------------------------------------------------ HMAC objects

fn hmac_state(name: &'static str, key: &[u8]) -> HmacState {
    let (mut inner, _) = make_digest(name).unwrap();
    let bs = inner.block_size();
    let mut k = if key.len() > bs {
        let (mut t, _) = make_digest(name).unwrap();
        t.update(key);
        t.digest()
    } else {
        key.to_vec()
    };
    k.resize(bs, 0);
    let (mut outer, _) = make_digest(name).unwrap();
    inner.update(&k.iter().map(|b| b ^ 0x36).collect::<Vec<u8>>());
    outer.update(&k.iter().map(|b| b ^ 0x5c).collect::<Vec<u8>>());
    HmacState { inner, outer, name: hmac_name(name) }
}

fn hmac_name(n: &str) -> &'static str {
    for a in ALGOS {
        if *a == n {
            return Box::leak(format!("hmac-{}", a).into_boxed_str());
        }
    }
    "hmac-unknown"
}

impl HmacState {
    fn finish(&self) -> Vec<u8> {
        let ih = self.inner.digest();
        let mut o = self.outer.box_clone();
        o.update(&ih);
        o.digest()
    }
}

fn m_update(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("update", &["msg"], 0, 1);
    let [m] = args::parse::<1>(&P, a, kw)?;
    let b = hash_data(m.unwrap())?;
    let mut st = cc::lock::<HmacState>(o)?;
    update_digest(&mut *st.inner, b.data());
    Ok(cc::none())
}
fn m_digest(o: &PyObject) -> PyResult {
    let d = cc::lock::<HmacState>(o)?.finish();
    cc::new_bytes(&d)
}
fn m_hexdigest(o: &PyObject) -> PyResult {
    let d = cc::lock::<HmacState>(o)?.finish();
    Ok(pystr::new_str(&h::hex(&d)))
}
fn m_copy(o: &PyObject) -> PyResult {
    let st = cc::lock::<HmacState>(o)?;
    let n = HmacState { inner: st.inner.box_clone(), outer: st.outer.box_clone(), name: st.name };
    drop(st);
    cc::new_obj(o.type_ptr(), n)
}
fn m_name(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(cc::lock::<HmacState>(o)?.name))
}
fn m_digest_size(o: &PyObject) -> PyResult {
    Ok(int::new_int(cc::lock::<HmacState>(o)?.outer.digest_size() as i64))
}
fn m_block_size(o: &PyObject) -> PyResult {
    Ok(int::new_int(cc::lock::<HmacState>(o)?.inner.block_size() as i64))
}
unsafe extern "C" fn m_repr_slot(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let r = (|| -> PyResult {
            let st = cc::lock::<HmacState>(o)?;
            Ok(pystr::new_str(&format!("<{} HMAC object @ {:#x}>", st.name.trim_start_matches("hmac-"), o.as_ptr() as usize)))
        })();
        into_c(r)
    }
}

// ------------------------------------------------------------------ digestmod resolution

/// Registry of constructor objects (by address) -> algorithm name.
static CTORS: std::sync::Mutex<Vec<(usize, &'static str)>> = std::sync::Mutex::new(Vec::new());

fn register_ctors(d: &PyObject, pairs: &[(&str, &'static str)]) -> PyResult<()> {
    let mut g = CTORS.lock().unwrap();
    for (fname, algo) in pairs {
        if let Some(f) = dict::get_item_rs(d, fname) {
            let p = f.as_ptr() as usize;
            std::mem::forget(f);
            g.push((p, algo));
        }
    }
    Ok(())
}

/// Resolve a `digestmod` argument to a canonical name; Ok(None) means "unsupported".
fn resolve_digestmod(dm: &PyObject) -> PyResult<Option<&'static str>> {
    if is_str(dm) {
        return Ok(canonical(&pystr::to_string_lossy(dm)));
    }
    let p = dm.as_ptr() as usize;
    let g = CTORS.lock().unwrap();
    Ok(g.iter().find(|(a, _)| *a == p).map(|(_, n)| *n))
}

fn unsupported(name: &str) -> PyErr {
    new_err(UNSUPPORTED.ptr(), &format!("unsupported hash type {}", name))
}

fn digestmod_repr(o: &PyObject) -> String {
    if is_str(o) { pystr::to_string_lossy(o) } else { crate::types::typeobject::tname(o) }
}

// ------------------------------------------------------------------ _hashlib functions

fn openssl_new_named(name: &'static str, data: Option<&PyObject>) -> PyResult {
    let (d, kind) = make_digest(name).unwrap();
    let t = if kind == Kind::Shake { T_HASHXOF } else { T_HASH };
    new_hash_obj(t, d, name, kind, data)
}

fn hl_new(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let v = parse_ctor("new", 2, &["name", "data"], a, kw)?;
    let Some(n) = v[0] else {
        raise!(TypeError, "new() missing required argument 'name' (pos 1)");
    };
    if !is_str(n) {
        raise!(TypeError, "new() argument 'name' must be str, not {}", tname(n));
    }
    let s = pystr::to_string_lossy(n);
    match canonical(&s) {
        Some(c) => openssl_new_named(c, v[1]),
        None => Err(unsupported(&s)),
    }
}

macro_rules! openssl_fn {
    ($f:ident, $n:expr, $fname:expr) => {
        fn $f(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
            let v = parse_ctor($fname, 1, &["data"], a, kw)?;
            openssl_new_named($n, v[0])
        }
    };
}
openssl_fn!(o_md5, "md5", "openssl_md5");
openssl_fn!(o_sha1, "sha1", "openssl_sha1");
openssl_fn!(o_sha224, "sha224", "openssl_sha224");
openssl_fn!(o_sha256, "sha256", "openssl_sha256");
openssl_fn!(o_sha384, "sha384", "openssl_sha384");
openssl_fn!(o_sha512, "sha512", "openssl_sha512");
openssl_fn!(o_sha3_224, "sha3_224", "openssl_sha3_224");
openssl_fn!(o_sha3_256, "sha3_256", "openssl_sha3_256");
openssl_fn!(o_sha3_384, "sha3_384", "openssl_sha3_384");
openssl_fn!(o_sha3_512, "sha3_512", "openssl_sha3_512");
openssl_fn!(o_shake_128, "shake_128", "openssl_shake_128");
openssl_fn!(o_shake_256, "shake_256", "openssl_shake_256");

fn bytes_arg(o: &PyObject) -> PyResult<Vec<u8>> {
    if is_str(o) {
        raise!(TypeError, "a bytes-like object is required, not 'str'");
    }
    Ok(cc::buffer(o)?.data().to_vec())
}

fn hl_pbkdf2(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("pbkdf2_hmac", &["hash_name", "password", "salt", "iterations", "dklen"], 0, 4);
    let [hn, pw, salt, it, dklen] = args::parse::<5>(&P, a, kw)?;
    let hn = hn.unwrap();
    if !is_str(hn) {
        raise!(TypeError, "pbkdf2_hmac() argument 'hash_name' must be str, not {}", tname(hn));
    }
    let pw = bytes_arg(pw.unwrap())?;
    let salt = bytes_arg(salt.unwrap())?;
    let it = ab::index(it.unwrap())?;
    let iters = match int::as_i64(&it) {
        Some(v) => v,
        None => raise!(OverflowError, "Python int too large to convert to C long"),
    };
    if iters < 1 {
        raise!(ValueError, "iteration value must be greater than 0.");
    }
    if iters > i32::MAX as i64 {
        raise!(OverflowError, "iteration value is too great.");
    }
    let name = pystr::to_string_lossy(hn);
    let Some(algo) = canonical(&name).filter(|a| !a.starts_with("shake")) else {
        return Err(unsupported(&name));
    };
    let probe = make_digest(algo).unwrap().0;
    let dklen = match dklen {
        Some(d) if !cc::is_none(d) => {
            let v = cc::ssize(d)?;
            if v < 1 {
                raise!(ValueError, "key length must be greater than 0.");
            }
            if v > i32::MAX as isize {
                raise!(OverflowError, "key length is too great.");
            }
            v as usize
        }
        _ => probe.digest_size(),
    };
    let mk = move || -> Box<dyn Digest> { make_digest(algo).unwrap().0 };
    let out = crate::threads::blocking(|| h::pbkdf2_hmac(&mk, &pw, &salt, iters as u64, dklen));
    cc::new_bytes(&out)
}

fn hl_scrypt(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params {
        fname: "scrypt",
        names: &["password", "salt", "n", "r", "p", "maxmem", "dklen"],
        posonly: 1,
        required: 1,
        kwonly: 6,
        kwonly_required: &["salt", "n", "r", "p"],
    };
    let [pw, salt, n, r, p, maxmem, dklen] = args::parse::<7>(&P, a, kw)?;
    let pw = bytes_arg(pw.unwrap())?;
    let Some(salt) = salt else {
        raise!(TypeError, "scrypt() missing required argument 'salt'");
    };
    let salt = bytes_arg(salt)?;
    let geti = |o: Option<&PyObject>, nm: &str| -> PyResult<i64> {
        let Some(o) = o else {
            raise!(TypeError, "scrypt() missing required argument '{}'", nm);
        };
        if cc::is_none(o) || !(int::is_int(o) || ab::has_index(o)) {
            raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(o));
        }
        let i = ab::index(o)?;
        match int::as_i64(&i) {
            Some(v) => Ok(v),
            None => raise!(OverflowError, "Python int too large to convert to C long"),
        }
    };
    let n = geti(n, "n")?;
    let r = geti(r, "r")?;
    let p = geti(p, "p")?;
    let maxmem = match maxmem {
        None => 0,
        o => geti(o, "maxmem")?,
    };
    let dklen = match dklen {
        None => 64,
        o => geti(o, "dklen")?,
    };
    if n < 2 || (n & (n - 1)) != 0 {
        raise!(ValueError, "n must be a power of 2 greater than 1");
    }
    if r < 1 || r > u32::MAX as i64 {
        raise!(ValueError, "r must be a positive integer");
    }
    if p < 1 || p > u32::MAX as i64 {
        raise!(ValueError, "p must be a positive integer");
    }
    if maxmem < 0 || maxmem > i32::MAX as i64 {
        raise!(ValueError, "maxmem must be positive and smaller than {}", i32::MAX);
    }
    let max_dk = ((1i64 << 32) - 1) * 32;
    if dklen < 1 || dklen > max_dk {
        raise!(ValueError, "dklen must be greater than 0 and smaller than {}", max_dk);
    }
    let limit = if maxmem == 0 { 32 * 1024 * 1024 } else { maxmem as u128 };
    let need = 128u128 * r as u128 * (n as u128 + p as u128 + 2);
    if need > limit || (r as u128) * (p as u128) >= (1u128 << 30) || n as u128 >= (1u128 << (16 * r.min(4) as u32)) && r < 4 && n > 65536 {
        raise!(ValueError, "Invalid parameter combination for n, r, p, maxmem.");
    }
    let out = crate::threads::blocking(|| h::scrypt(&pw, &salt, n as u64, r as usize, p as usize, dklen as usize));
    cc::new_bytes(&out)
}

fn hl_hmac_digest(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("hmac_digest", &["key", "msg", "digest"], 0, 2);
    let [key, msg, dm] = args::parse::<3>(&P, a, kw)?;
    let key = bytes_arg(key.unwrap())?;
    let msg = hash_data(msg.unwrap())?;
    let Some(dm) = dm else {
        raise!(TypeError, "Missing required parameter 'digest'.");
    };
    let name = match resolve_digestmod(dm)? {
        Some(n) if !n.starts_with("shake") => n,
        _ => return Err(unsupported(&digestmod_repr(dm))),
    };
    let mut st = hmac_state(name, &key);
    update_digest(&mut *st.inner, msg.data());
    cc::new_bytes(&st.finish())
}

fn hl_hmac_new(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("hmac_new", &["key", "msg", "digestmod"], 0, 1);
    let [key, msg, dm] = args::parse::<3>(&P, a, kw)?;
    hmac_new_common(T_OHMAC, key.unwrap(), msg, dm, &UNSUPPORTED)
}

fn hmac_new_common(t: usize, key: &PyObject, msg: Option<&PyObject>, dm: Option<&PyObject>, exc: &ExcCell) -> PyResult {
    let key = bytes_arg(key)?;
    let dm = match dm {
        Some(d) if !cc::is_none(d) || t == T_BHMAC => d,
        _ => raise!(TypeError, "Missing required parameter 'digestmod'."),
    };
    let name = match resolve_digestmod(dm)? {
        Some(n) if !n.starts_with("shake") => n,
        _ => {
            if t == T_BHMAC && cc::is_none(dm) {
                raise!(TypeError, "Missing required parameter 'digestmod'.");
            }
            return Err(new_err(exc.ptr(), &format!("unsupported hash type {}", digestmod_repr(dm))));
        }
    };
    let mut st = hmac_state(name, &key);
    if let Some(m) = msg {
        if !cc::is_none(m) {
            let b = hash_data(m)?;
            update_digest(&mut *st.inner, b.data());
        }
    }
    cc::new_obj(tp(t), st)
}

fn hl_get_fips_mode(_m: &PyObject) -> PyResult {
    Ok(int::new_int(0))
}

// ------------------------------------------------------------------ _hmac (builtin HMAC)

fn bh_new(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("new", &["key", "msg", "digestmod"], 0, 1);
    let [key, msg, dm] = args::parse::<3>(&P, a, kw)?;
    hmac_new_common(T_BHMAC, key.unwrap(), msg, dm, &UNKNOWN_HASH)
}

fn bh_compute(key: &PyObject, msg: &PyObject, name: &'static str) -> PyResult {
    let key = bytes_arg(key)?;
    let msg = hash_data(msg)?;
    let mut st = hmac_state(name, &key);
    update_digest(&mut *st.inner, msg.data());
    cc::new_bytes(&st.finish())
}

fn bh_compute_digest(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("compute_digest", &["key", "msg", "digest"], 0, 3);
    let [key, msg, dm] = args::parse::<3>(&P, a, kw)?;
    let dm = dm.unwrap();
    let name = match resolve_digestmod(dm)? {
        Some(n) if !n.starts_with("shake") => n,
        _ => return Err(new_err(UNKNOWN_HASH.ptr(), &format!("unsupported hash type {}", digestmod_repr(dm)))),
    };
    bh_compute(key.unwrap(), msg.unwrap(), name)
}

macro_rules! compute_fn {
    ($f:ident, $n:expr) => {
        fn $f(_m: &PyObject, a: &[&PyObject]) -> PyResult {
            args::check_pos(concat!("compute_", $n), a, 2, 2)?;
            bh_compute(a[0], a[1], $n)
        }
    };
}
compute_fn!(bh_md5, "md5");
compute_fn!(bh_sha1, "sha1");
compute_fn!(bh_sha224, "sha224");
compute_fn!(bh_sha256, "sha256");
compute_fn!(bh_sha384, "sha384");
compute_fn!(bh_sha512, "sha512");
compute_fn!(bh_sha3_224, "sha3_224");
compute_fn!(bh_sha3_256, "sha3_256");
compute_fn!(bh_sha3_384, "sha3_384");
compute_fn!(bh_sha3_512, "sha3_512");
compute_fn!(bh_blake2s, "blake2s");
compute_fn!(bh_blake2b, "blake2b");

// ------------------------------------------------------------------ type setup

static INIT: std::sync::Once = std::sync::Once::new();
static mut INIT_ERR: bool = false;

unsafe fn setup_hash_type(i: usize, name: &'static std::ffi::CStr, xof: bool, sha3: bool, instantiable: bool, new: newfunc) -> PyResult<()> {
    unsafe {
        let flags = Py_TPFLAGS_IMMUTABLETYPE as u64 | if instantiable { 0 } else { Py_TPFLAGS_DISALLOW_INSTANTIATION as u64 };
        cc::init_state_type::<HState>(tp(i), name, flags);
        let t = &mut *tp(i);
        t.tp_methods = if xof {
            crate::mdefs![
                ("update", O, h_update, "update($self, data, /)\n--\n\nUpdate this hash object's state with the provided string."),
                ("digest", FASTKW, x_digest, "digest($self, /, length)\n--\n\nReturn the digest value as a bytes object."),
                ("hexdigest", FASTKW, x_hexdigest, "hexdigest($self, /, length)\n--\n\nReturn the digest value as a string of hexadecimal digits."),
                ("copy", NOARGS, h_copy, "copy($self, /)\n--\n\nReturn a copy of the hash object."),
                ("__reduce__", NOARGS, cc::no_pickle),
            ]
        } else {
            crate::mdefs![
                ("update", O, h_update, "update($self, data, /)\n--\n\nUpdate this hash object's state with the provided string."),
                ("digest", NOARGS, h_digest, "digest($self, /)\n--\n\nReturn the digest value as a bytes object."),
                ("hexdigest", NOARGS, h_hexdigest, "hexdigest($self, /)\n--\n\nReturn the digest value as a string of hexadecimal digits."),
                ("copy", NOARGS, h_copy, "copy($self, /)\n--\n\nReturn a copy of the hash object."),
                ("__reduce__", NOARGS, cc::no_pickle),
            ]
        };
        t.tp_getset = if sha3 {
            crate::getset_table![
                ("name", h_name),
                ("digest_size", h_digest_size),
                ("block_size", h_block_size),
                ("_capacity_bits", h_capacity_bits),
                ("_rate_bits", h_rate_bits),
                ("_suffix", h_suffix),
            ]
        } else {
            crate::getset_table![("name", h_name), ("digest_size", h_digest_size), ("block_size", h_block_size)]
        };
        if i == T_HASH || i == T_HASHXOF {
            t.tp_repr = Some(h_repr_slot);
        }
        t.tp_new = new;
        type_ready(tp(i))?;
        if !instantiable {
            (*tp(i)).tp_new = None;
        }
        Ok(())
    }
}

unsafe fn setup_hmac_type(i: usize, name: &'static std::ffi::CStr) -> PyResult<()> {
    unsafe {
        cc::init_state_type::<HmacState>(tp(i), name, (Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_DISALLOW_INSTANTIATION) as u64);
        let t = &mut *tp(i);
        t.tp_methods = crate::mdefs![
            ("update", FASTKW, m_update, "update($self, /, msg)\n--\n\nUpdate the HMAC object with msg."),
            ("digest", NOARGS, m_digest, "digest($self, /)\n--\n\nReturn the digest of the bytes passed to the update() method so far."),
            ("hexdigest", NOARGS, m_hexdigest, "hexdigest($self, /)\n--\n\nReturn hexadecimal digest of the bytes passed to the update() method so far."),
            ("copy", NOARGS, m_copy, "copy($self, /)\n--\n\nReturn a copy (\"clone\") of the HMAC object."),
            ("__reduce__", NOARGS, cc::no_pickle),
        ];
        t.tp_getset = crate::getset_table![("name", m_name), ("digest_size", m_digest_size), ("block_size", m_block_size)];
        t.tp_repr = Some(m_repr_slot);
        type_ready(tp(i))?;
        (*tp(i)).tp_new = None;
        Ok(())
    }
}

fn init_types() -> PyResult<()> {
    INIT.call_once(|| unsafe {
        let r = (|| -> PyResult<()> {
            setup_hash_type(T_MD5, c"_md5.md5", false, false, false, None)?;
            setup_hash_type(T_SHA1, c"_sha1.sha1", false, false, false, None)?;
            setup_hash_type(T_SHA224, c"_sha2.SHA224Type", false, false, false, None)?;
            setup_hash_type(T_SHA256, c"_sha2.SHA256Type", false, false, false, None)?;
            setup_hash_type(T_SHA384, c"_sha2.SHA384Type", false, false, false, None)?;
            setup_hash_type(T_SHA512, c"_sha2.SHA512Type", false, false, false, None)?;
            setup_hash_type(T_SHA3_224, c"_sha3.sha3_224", false, true, true, Some(sha3_new))?;
            setup_hash_type(T_SHA3_224 + 1, c"_sha3.sha3_256", false, true, true, Some(sha3_new))?;
            setup_hash_type(T_SHA3_224 + 2, c"_sha3.sha3_384", false, true, true, Some(sha3_new))?;
            setup_hash_type(T_SHA3_224 + 3, c"_sha3.sha3_512", false, true, true, Some(sha3_new))?;
            setup_hash_type(T_SHAKE128, c"_sha3.shake_128", true, true, true, Some(sha3_new))?;
            setup_hash_type(T_SHAKE256, c"_sha3.shake_256", true, true, true, Some(sha3_new))?;
            setup_hash_type(T_BLAKE2B, c"_blake2.blake2b", false, false, true, Some(blake2_new))?;
            setup_hash_type(T_BLAKE2S, c"_blake2.blake2s", false, false, true, Some(blake2_new))?;
            setup_hash_type(T_HASH, c"_hashlib.HASH", false, false, false, None)?;
            setup_hash_type(T_HASHXOF, c"_hashlib.HASHXOF", true, false, false, None)?;
            setup_hmac_type(T_OHMAC, c"_hashlib.HMAC")?;
            setup_hmac_type(T_BHMAC, c"_hmac.HMAC")?;
            // blake2 class constants
            for (i, salt, pers, key, dig) in [(T_BLAKE2B, 16, 16, 64, 64), (T_BLAKE2S, 8, 8, 32, 32)] {
                let d = PyRef::from_borrowed((*tp(i)).tp_dict);
                cc::set_int(&d, "SALT_SIZE", salt)?;
                cc::set_int(&d, "PERSON_SIZE", pers)?;
                cc::set_int(&d, "MAX_KEY_SIZE", key)?;
                cc::set_int(&d, "MAX_DIGEST_SIZE", dig)?;
            }
            Ok(())
        })();
        if r.is_err() {
            INIT_ERR = true;
        }
    });
    if unsafe { INIT_ERR } {
        raise!(SystemError, "failed to initialize hash types");
    }
    Ok(())
}

fn new_mod(name: &str) -> PyResult<(PyRef, PyRef)> {
    init_types()?;
    let m = crate::types::module::new_module(name)?;
    let d = crate::types::module::module_dict(&m);
    Ok((m, d))
}

fn add_type(d: &PyObject, name: &str, i: usize) -> PyResult<()> {
    dict::set_item_str(d, name, &type_ref(tp(i)))
}

pub fn init_md5() -> PyResult {
    let (m, d) = new_mod("_md5")?;
    add_type(&d, "MD5Type", T_MD5)?;
    cc::set_int(&d, "_GIL_MINSIZE", GIL_MINSIZE)?;
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![("md5", FASTKW, c_md5, "md5($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturn a new MD5 hash object; optionally initialized with a string.")],
        Some(&m),
    )?;
    register_ctors(&d, &[("md5", "md5")])?;
    Ok(m)
}

pub fn init_sha1() -> PyResult {
    let (m, d) = new_mod("_sha1")?;
    add_type(&d, "SHA1Type", T_SHA1)?;
    cc::set_int(&d, "_GIL_MINSIZE", GIL_MINSIZE)?;
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![("sha1", FASTKW, c_sha1, "sha1($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturn a new SHA1 hash object; optionally initialized with a string.")],
        Some(&m),
    )?;
    register_ctors(&d, &[("sha1", "sha1")])?;
    Ok(m)
}

pub fn init_sha2() -> PyResult {
    let (m, d) = new_mod("_sha2")?;
    add_type(&d, "SHA224Type", T_SHA224)?;
    add_type(&d, "SHA256Type", T_SHA256)?;
    add_type(&d, "SHA384Type", T_SHA384)?;
    add_type(&d, "SHA512Type", T_SHA512)?;
    cc::set_int(&d, "_GIL_MINSIZE", GIL_MINSIZE)?;
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("sha224", FASTKW, c_sha224, "sha224($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturn a new SHA-224 hash object; optionally initialized with a string."),
            ("sha256", FASTKW, c_sha256, "sha256($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturn a new SHA-256 hash object; optionally initialized with a string."),
            ("sha384", FASTKW, c_sha384, "sha384($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturn a new SHA-384 hash object; optionally initialized with a string."),
            ("sha512", FASTKW, c_sha512, "sha512($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturn a new SHA-512 hash object; optionally initialized with a string."),
        ],
        Some(&m),
    )?;
    register_ctors(&d, &[("sha224", "sha224"), ("sha256", "sha256"), ("sha384", "sha384"), ("sha512", "sha512")])?;
    Ok(m)
}

pub fn init_sha3() -> PyResult {
    let (m, d) = new_mod("_sha3")?;
    for (n, i) in [("sha3_224", 0), ("sha3_256", 1), ("sha3_384", 2), ("sha3_512", 3), ("shake_128", 4), ("shake_256", 5)] {
        add_type(&d, n, T_SHA3_224 + i)?;
    }
    cc::set_str(&d, "implementation", "rust")?;
    cc::set_int(&d, "_GIL_MINSIZE", GIL_MINSIZE)?;
    register_ctors(
        &d,
        &[
            ("sha3_224", "sha3_224"),
            ("sha3_256", "sha3_256"),
            ("sha3_384", "sha3_384"),
            ("sha3_512", "sha3_512"),
            ("shake_128", "shake_128"),
            ("shake_256", "shake_256"),
        ],
    )?;
    Ok(m)
}

pub fn init_blake2() -> PyResult {
    let (m, d) = new_mod("_blake2")?;
    add_type(&d, "blake2b", T_BLAKE2B)?;
    add_type(&d, "blake2s", T_BLAKE2S)?;
    for (k, v) in [
        ("BLAKE2B_SALT_SIZE", 16),
        ("BLAKE2B_PERSON_SIZE", 16),
        ("BLAKE2B_MAX_KEY_SIZE", 64),
        ("BLAKE2B_MAX_DIGEST_SIZE", 64),
        ("BLAKE2S_SALT_SIZE", 8),
        ("BLAKE2S_PERSON_SIZE", 8),
        ("BLAKE2S_MAX_KEY_SIZE", 32),
        ("BLAKE2S_MAX_DIGEST_SIZE", 32),
        ("_GIL_MINSIZE", GIL_MINSIZE),
    ] {
        cc::set_int(&d, k, v)?;
    }
    register_ctors(&d, &[("blake2b", "blake2b"), ("blake2s", "blake2s")])?;
    Ok(m)
}

fn compare_digest_obj() -> PyResult {
    let op = crate::import::import_absolute("_operator")?;
    ab::getattr_str(&op, "_compare_digest")
}

pub fn init_hashlib() -> PyResult {
    let (m, d) = new_mod("_hashlib")?;
    let e = UNSUPPORTED.get_or_create("_hashlib", "UnsupportedDigestmodError", crate::types::exceptions::ValueError())?;
    dict::set_item_str(&d, "UnsupportedDigestmodError", &type_ref(e))?;
    add_type(&d, "HASH", T_HASH)?;
    add_type(&d, "HASHXOF", T_HASHXOF)?;
    add_type(&d, "HMAC", T_OHMAC)?;
    cc::set_int(&d, "_GIL_MINSIZE", GIL_MINSIZE)?;
    let ds = |s: &'static str| s;
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("new", FASTKW, hl_new, ds("new($module, /, name, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturn a new hash object using the named algorithm.")),
            ("openssl_md5", FASTKW, o_md5, "openssl_md5($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturns a md5 hash object; optionally initialized with a string"),
            ("openssl_sha1", FASTKW, o_sha1, "openssl_sha1($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturns a sha1 hash object; optionally initialized with a string"),
            ("openssl_sha224", FASTKW, o_sha224, "openssl_sha224($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturns a sha224 hash object; optionally initialized with a string"),
            ("openssl_sha256", FASTKW, o_sha256, "openssl_sha256($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturns a sha256 hash object; optionally initialized with a string"),
            ("openssl_sha384", FASTKW, o_sha384, "openssl_sha384($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturns a sha384 hash object; optionally initialized with a string"),
            ("openssl_sha512", FASTKW, o_sha512, "openssl_sha512($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturns a sha512 hash object; optionally initialized with a string"),
            ("openssl_sha3_224", FASTKW, o_sha3_224, "openssl_sha3_224($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturns a sha3-224 hash object; optionally initialized with a string"),
            ("openssl_sha3_256", FASTKW, o_sha3_256, "openssl_sha3_256($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturns a sha3-256 hash object; optionally initialized with a string"),
            ("openssl_sha3_384", FASTKW, o_sha3_384, "openssl_sha3_384($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturns a sha3-384 hash object; optionally initialized with a string"),
            ("openssl_sha3_512", FASTKW, o_sha3_512, "openssl_sha3_512($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturns a sha3-512 hash object; optionally initialized with a string"),
            ("openssl_shake_128", FASTKW, o_shake_128, "openssl_shake_128($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturns a shake-128 variable hash object; optionally initialized with a string"),
            ("openssl_shake_256", FASTKW, o_shake_256, "openssl_shake_256($module, /, data=b'', *, usedforsecurity=True, string=None)\n--\n\nReturns a shake-256 variable hash object; optionally initialized with a string"),
            ("pbkdf2_hmac", FASTKW, hl_pbkdf2, "pbkdf2_hmac($module, /, hash_name, password, salt, iterations, dklen=None)\n--\n\nPassword based key derivation function 2 (PKCS #5 v2.0) with HMAC as pseudorandom function."),
            ("scrypt", FASTKW, hl_scrypt, "scrypt($module, /, password, *, salt, n, r, p, maxmem=0, dklen=64)\n--\n\nscrypt password-based key derivation function."),
            ("hmac_digest", FASTKW, hl_hmac_digest, "hmac_digest($module, /, key, msg, digest)\n--\n\nSingle-shot HMAC."),
            ("hmac_new", FASTKW, hl_hmac_new, "hmac_new($module, /, key, msg=b'', digestmod=None)\n--\n\nReturn a new hmac object."),
            ("get_fips_mode", NOARGS, hl_get_fips_mode, "get_fips_mode($module, /)\n--\n\nDetermine the OpenSSL FIPS mode of operation."),
        ],
        Some(&m),
    )?;
    let cd = compare_digest_obj()?;
    dict::set_item_str(&d, "compare_digest", &cd)?;
    let pairs: [(&str, &'static str); 12] = [
        ("openssl_md5", "md5"),
        ("openssl_sha1", "sha1"),
        ("openssl_sha224", "sha224"),
        ("openssl_sha256", "sha256"),
        ("openssl_sha384", "sha384"),
        ("openssl_sha512", "sha512"),
        ("openssl_sha3_224", "sha3_224"),
        ("openssl_sha3_256", "sha3_256"),
        ("openssl_sha3_384", "sha3_384"),
        ("openssl_sha3_512", "sha3_512"),
        ("openssl_shake_128", "shake_128"),
        ("openssl_shake_256", "shake_256"),
    ];
    register_ctors(&d, &pairs)?;
    let cons = dict::new_dict();
    for (f, n) in pairs {
        if let Some(fo) = dict::get_item_rs(&d, f) {
            dict::set_item(&cons, &fo, &pystr::new_str(n))?;
        }
    }
    let proxy = crate::types::mappingproxy::new_mappingproxy(&cons)?;
    dict::set_item_str(&d, "_constructors", &proxy)?;
    let names: Vec<PyRef> = ALGOS.iter().map(|a| pystr::new_str(a)).collect();
    let fs = crate::types::set::new_frozenset_from_vec(names)?;
    dict::set_item_str(&d, "openssl_md_meth_names", &fs)?;
    let _ = tuple::new_tuple;
    let _ = bool_::new_bool;
    Ok(m)
}

pub fn init_hmac() -> PyResult {
    let (m, d) = new_mod("_hmac")?;
    let e = UNKNOWN_HASH.get_or_create("_hmac", "UnknownHashError", crate::types::exceptions::ValueError())?;
    dict::set_item_str(&d, "UnknownHashError", &type_ref(e))?;
    add_type(&d, "HMAC", T_BHMAC)?;
    cc::set_int(&d, "_GIL_MINSIZE", GIL_MINSIZE)?;
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("new", FASTKW, bh_new, "new($module, /, key, msg=None, digestmod=None)\n--\n\nReturn a new HMAC object."),
            ("compute_digest", FASTKW, bh_compute_digest, "compute_digest($module, /, key, msg, digest)\n--\n\n"),
            ("compute_md5", FAST, bh_md5, "compute_md5($module, key, msg, /)\n--\n\n"),
            ("compute_sha1", FAST, bh_sha1, "compute_sha1($module, key, msg, /)\n--\n\n"),
            ("compute_sha224", FAST, bh_sha224, "compute_sha224($module, key, msg, /)\n--\n\n"),
            ("compute_sha256", FAST, bh_sha256, "compute_sha256($module, key, msg, /)\n--\n\n"),
            ("compute_sha384", FAST, bh_sha384, "compute_sha384($module, key, msg, /)\n--\n\n"),
            ("compute_sha512", FAST, bh_sha512, "compute_sha512($module, key, msg, /)\n--\n\n"),
            ("compute_sha3_224", FAST, bh_sha3_224, "compute_sha3_224($module, key, msg, /)\n--\n\n"),
            ("compute_sha3_256", FAST, bh_sha3_256, "compute_sha3_256($module, key, msg, /)\n--\n\n"),
            ("compute_sha3_384", FAST, bh_sha3_384, "compute_sha3_384($module, key, msg, /)\n--\n\n"),
            ("compute_sha3_512", FAST, bh_sha3_512, "compute_sha3_512($module, key, msg, /)\n--\n\n"),
            ("compute_blake2s_32", FAST, bh_blake2s, "compute_blake2s_32($module, key, msg, /)\n--\n\n"),
            ("compute_blake2b_32", FAST, bh_blake2b, "compute_blake2b_32($module, key, msg, /)\n--\n\n"),
        ],
        Some(&m),
    )?;
    Ok(m)
}
