//! CJK multibyte codecs: `_multibytecodec` (the generic codec objects and
//! their incremental / stream variants) and the per-region codec modules
//! `_codecs_cn`, `_codecs_jp`, `_codecs_kr`, `_codecs_tw`, `_codecs_hk`,
//! `_codecs_iso2022`.
//!
//! Every codec is a `CodecDef`: a name plus an implementation of `MbCodec`,
//! which converts as much input as it can and reports the first problem as a
//! `Fail`; the driver in `mbc.rs` applies the error handler and resumes.

pub mod table;
#[rustfmt::skip]
pub mod data_cn;
#[rustfmt::skip]
pub mod data_hk;
#[rustfmt::skip]
pub mod data_jp;
#[rustfmt::skip]
pub mod data_kr;
#[rustfmt::skip]
pub mod data_tw;
pub mod dbcs;
pub mod iso2022;
pub mod mbc;

use crate::errors::*;
use crate::ffi::PyObject;
use crate::raise;
use crate::object::*;

/// Codec state (8 bytes, exposed by getstate()/setstate()).
pub type St = [u8; 8];

/// Why a codec stopped before the end of its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fail {
    /// The next `n` input units cannot be converted.
    Illegal(usize),
    /// The input ends inside a sequence (more input may complete it).
    TooFew,
    /// A codec bug or an impossible state.
    Internal,
}

pub type R = Result<(), Fail>;

pub trait MbCodec: Sync + Send {
    fn enc_init(&self, st: &mut St) {
        *st = [0; 8];
    }
    /// Encode `inp[*pos..]`, advancing `*pos`.  `flush`: no more input follows,
    /// so a character that might start a combining pair must be written now.
    fn encode(&self, st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, flush: bool) -> R;
    /// Return to the initial shift state (stateful encodings only).
    fn enc_reset(&self, _st: &mut St, _out: &mut Vec<u8>) {}
    fn dec_init(&self, st: &mut St) {
        *st = [0; 8];
    }
    fn decode(&self, st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R;
}

pub struct CodecDef {
    pub name: &'static str,
    pub imp: &'static dyn MbCodec,
}

fn find(list: &'static [CodecDef], name: &PyObject) -> PyResult<&'static CodecDef> {
    if !crate::types::str::is_str(name) {
        raise!(TypeError, "getcodec() argument must be str, not {}", crate::types::typeobject::tname(name));
    }
    let s = crate::types::str::to_string_lossy(name);
    match list.iter().find(|c| c.name == s) {
        Some(c) => Ok(c),
        None => raise!(LookupError, "no such codec is supported."),
    }
}

macro_rules! region {
    ($init:ident, $get:ident, $modname:literal, $list:expr) => {
        fn $get(_m: &PyObject, name: &PyObject) -> PyResult {
            mbc::new_codec(find($list, name)?)
        }
        pub fn $init() -> PyResult {
            let m = crate::types::module::new_module($modname)?;
            let d = crate::types::module::module_dict(&m);
            crate::builtins::add_methods_to_dict(&d, crate::mdefs![("getcodec", O, $get, "getcodec($module, encoding, /)\n--\n\n")], Some(&m))?;
            Ok(m)
        }
    };
}

region!(init_cn, getcodec_cn, "_codecs_cn", dbcs::CN);
region!(init_jp, getcodec_jp, "_codecs_jp", dbcs::JP);
region!(init_kr, getcodec_kr, "_codecs_kr", dbcs::KR);
region!(init_tw, getcodec_tw, "_codecs_tw", dbcs::TW);
region!(init_hk, getcodec_hk, "_codecs_hk", dbcs::HK);
region!(init_iso2022, getcodec_iso2022, "_codecs_iso2022", iso2022::CODECS);

pub fn register() {
    use crate::import::register_builtin as reg;
    reg("_multibytecodec", mbc::init_module);
    reg("_codecs_cn", init_cn);
    reg("_codecs_jp", init_jp);
    reg("_codecs_kr", init_kr);
    reg("_codecs_tw", init_tw);
    reg("_codecs_hk", init_hk);
    reg("_codecs_iso2022", init_iso2022);
}
