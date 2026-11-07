//! (perf6) The start-up image: frozen modules as code objects ready to run.
//!
//! HotSpot keeps the classes every JVM start loads in a class-data-sharing archive, so a
//! start maps them instead of parsing and verifying class files. Here the build script
//! (`build/main.rs`) compiles the frozen modules (importlib's bootstrap, os, site, ...) with
//! our compiler and translates every code object into the internal instruction form, and
//! this module turns an image record into code objects directly: no file access, no path
//! search, no pyc unmarshalling, no compilation. The translated form of a code object stays
//! in the image until the code first runs (`types::code::build_exec` decodes it then).
//!
//! The image is used for frozen modules whenever frozen modules are in use (`frozen.rs`
//! decides: on by default for an installed layout, as in CPython; off when running from the
//! source tree, where the files on disk are authoritative).

pub mod fmt;

use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::types::{bytes, int, none, str as pystr, tuple};
use fmt::In;
use std::sync::atomic::{AtomicPtr, Ordering};

static IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/startup_image.bin"));

struct Index {
    /// (kind, offset of the data, length) per string
    strings: Vec<(u8, u32, u32)>,
    /// the str objects, created (interned) on first use and kept for the process
    objs: Box<[AtomicPtr<PyObject>]>,
    /// (path below Lib/, offset of the code record)
    modules: Vec<(&'static str, usize)>,
    /// start of the translation areas and the length of area 0
    tx_base: usize,
    tx_a_len: usize,
}

fn index() -> &'static Index {
    static INDEX: std::sync::OnceLock<Index> = std::sync::OnceLock::new();
    INDEX.get_or_init(|| {
        let mut r = In::new(IMAGE, 0);
        assert_eq!(r.take(8), fmt::MAGIC, "startup image: bad magic");
        let n = r.n();
        let mut strings = Vec::with_capacity(n);
        for _ in 0..n {
            let kind = r.byte();
            let len = r.n();
            let at = r.pos;
            r.take(if kind == fmt::S_CPS { 4 * len } else { len });
            strings.push((kind, at as u32, len as u32));
        }
        let objs = (0..n).map(|_| AtomicPtr::new(std::ptr::null_mut())).collect();
        let nmod = r.n();
        let mut modules = Vec::with_capacity(nmod);
        for _ in 0..nmod {
            let e = r.take(12);
            let sid = u32::from_le_bytes(e[0..4].try_into().unwrap()) as usize;
            let off = u64::from_le_bytes(e[4..12].try_into().unwrap()) as usize;
            let (_, at, len) = strings[sid];
            let path = std::str::from_utf8(&IMAGE[at as usize..(at + len) as usize]).unwrap_or("");
            modules.push((path, off));
        }
        let t = r.take(16);
        let tx_base = u64::from_le_bytes(t[0..8].try_into().unwrap()) as usize;
        let tx_a_len = u64::from_le_bytes(t[8..16].try_into().unwrap()) as usize;
        Index { strings, objs, modules, tx_base, tx_a_len }
    })
}

/// Is there an image record for the frozen source `path` (below Lib/)?
pub fn has(path: &str) -> bool {
    !path.is_empty() && index().modules.iter().any(|(p, _)| *p == path)
}

/// String `id` of the image as an interned str.
fn string(ix: &Index, id: usize) -> PyRef {
    let p = ix.objs[id].load(Ordering::Acquire);
    if !p.is_null() {
        return unsafe { PyRef::from_borrowed(p) };
    }
    let (kind, at, len) = ix.strings[id];
    let s = if kind == fmt::S_ASCII {
        pystr::intern_latin1(&IMAGE[at as usize..(at + len) as usize])
    } else if kind == fmt::S_UTF8 {
        let b = &IMAGE[at as usize..(at + len) as usize];
        match std::str::from_utf8(b) {
            Ok(s) => pystr::intern_str(s),
            Err(_) => pystr::new_str(""),
        }
    } else {
        let b = &IMAGE[at as usize..at as usize + 4 * len as usize];
        let cps: Vec<u32> = b.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
        pystr::intern(pystr::from_codepoints(&cps))
    };
    match ix.objs[id].compare_exchange(std::ptr::null_mut(), s.as_ptr(), Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {
            // the table keeps its own reference
            std::mem::forget(s.clone());
            s
        }
        Err(other) => unsafe { PyRef::from_borrowed(other) },
    }
}

fn blob_obj(b: &[u8]) -> PyRef {
    bytes::bytes_singleton(b).unwrap_or_else(|| bytes::new_bytes(b))
}

fn konst(ix: &Index, r: &mut In<'static>, filename: &PyRef) -> PyResult {
    use fmt::*;
    Ok(match r.byte() {
        C_NONE => none::none_ref(),
        C_TRUE => crate::types::bool_::new_bool(true),
        C_FALSE => crate::types::bool_::new_bool(false),
        C_ELLIPSIS => none::ellipsis().to_ref(),
        C_INT => int::new_int(r.i()),
        C_BIGINT => {
            let neg = r.byte() != 0;
            let id = r.n();
            let (_, at, len) = ix.strings[id];
            let dec = std::str::from_utf8(&IMAGE[at as usize..(at + len) as usize]).unwrap_or("0");
            crate::types::int_methods::from_decimal_str(neg, dec)?
        }
        C_FLOAT => crate::types::float::new_float(f64::from_bits(u64::from_le_bytes(r.take(8).try_into().unwrap()))),
        C_COMPLEX => {
            let re = f64::from_bits(u64::from_le_bytes(r.take(8).try_into().unwrap()));
            let im = f64::from_bits(u64::from_le_bytes(r.take(8).try_into().unwrap()));
            crate::types::complex::new_complex(re, im)
        }
        C_STR => string(ix, r.n()),
        C_BYTES => blob_obj(r.blob()),
        t @ (C_TUPLE | C_FROZENSET) => {
            let n = r.n();
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(konst(ix, r, filename)?);
            }
            if t == C_TUPLE { tuple::new_tuple(v) } else { crate::types::set::new_frozenset_from_vec(v)? }
        }
        C_SLICE => {
            let a = konst(ix, r, filename)?;
            let b = konst(ix, r, filename)?;
            let c = konst(ix, r, filename)?;
            crate::types::slice::new_slice(&a, &b, &c)
        }
        C_CODE => code(ix, r, filename)?,
        t => crate::raise!(SystemError, "startup image: bad constant tag {}", t),
    })
}

fn code(ix: &Index, r: &mut In<'static>, filename: &PyRef) -> PyResult {
    let argcount = r.n() as i32;
    let posonly = r.n() as i32;
    let kwonly = r.n() as i32;
    let stacksize = r.n() as i32;
    let flags = r.n() as i32;
    let firstlineno = r.n() as i32;
    let co_code = bytes::new_bytes(r.blob());
    let nconsts = r.n();
    let mut consts = Vec::with_capacity(nconsts);
    for _ in 0..nconsts {
        consts.push(konst(ix, r, filename)?);
    }
    let n = r.n();
    let names: Vec<PyRef> = (0..n).map(|_| string(ix, r.n())).collect();
    let n = r.n();
    let localsplusnames: Vec<PyRef> = (0..n).map(|_| string(ix, r.n())).collect();
    let kinds = r.blob().to_vec();
    let name = string(ix, r.n());
    let qualname = string(ix, r.n());
    let linetable = blob_obj(r.blob());
    let exceptiontable = blob_obj(r.blob());
    let tx_ref = r.n();
    let tx_len = r.n();
    let tx: &'static [u8] = if tx_len == 0 {
        &[]
    } else {
        let start = ix.tx_base + if tx_ref & 1 != 0 { ix.tx_a_len } else { 0 } + (tx_ref >> 1);
        &IMAGE[start..start + tx_len]
    };
    let parts = crate::types::code::PublicParts {
        argcount,
        posonlyargcount: posonly,
        kwonlyargcount: kwonly,
        stacksize,
        flags,
        firstlineno,
        code: co_code,
        consts,
        names,
        localsplusnames,
        kinds,
        filename: filename.clone(),
        name,
        qualname,
        linetable,
        exceptiontable,
    };
    Ok(crate::types::code::new_code_from_image(parts, if tx.is_empty() { None } else { Some(tx) }))
}

/// The module code object of frozen source `path` (below Lib/), with `filename` as
/// co_filename of every code object. None if the image has no record for it.
pub fn load(path: &str, filename: &str) -> Option<PyResult> {
    let ix = index();
    let &(_, off) = ix.modules.iter().find(|(p, _)| *p == path)?;
    let mut r = In::new(IMAGE, off);
    let filename = pystr::intern_str(filename);
    Some(code(ix, &mut r, &filename))
}

/// Decoded translated form of a code object (see `fmt`).
pub struct Translation {
    pub code: Vec<u32>,
    pub exception_table: Vec<rpy_compiler::bytecode::ExceptionEntry>,
    pub extra_consts: Vec<crate::interp::translate::Extra>,
    pub pub_of_internal: Vec<u32>,
    pub internal_of_pub: Vec<u32>,
    pub eager_prologue: bool,
    pub gen_start: u32,
    pub pub_lines: Vec<i32>,
}

pub fn decode_translation(tx: &'static [u8]) -> Translation {
    use crate::interp::translate::Extra;
    let mut r = In::new(tx, 0);
    let n = r.n();
    let mut code: Vec<u32> = r.take(4 * n).chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    if !crate::interp::borrow::enabled() {
        // (refs1) the image was translated with borrowed loads (build script)
        use rpy_compiler::bytecode::Op;
        for w in code.iter_mut() {
            if *w & 0xff == Op::LoadFastBorrow as u32 {
                *w = (*w & !0xff) | Op::LoadFast as u32;
            }
        }
    }
    let ne = r.n();
    let mut exception_table = Vec::with_capacity(ne);
    for _ in 0..ne {
        let start = r.n() as u32;
        let end = r.n() as u32;
        let target = r.n() as u32;
        let depth = r.n() as u32;
        let lasti = r.n() != 0;
        exception_table.push(rpy_compiler::bytecode::ExceptionEntry { start, end, target, depth, lasti });
    }
    let nx = r.n();
    let mut extra_consts = Vec::with_capacity(nx);
    for _ in 0..nx {
        extra_consts.push(match r.byte() {
            fmt::X_INT => Extra::Int(r.i()),
            fmt::X_EMPTYSTR => Extra::EmptyStr,
            fmt::X_TRUE => Extra::True,
            fmt::X_FALSE => Extra::False,
            _ => Extra::None,
        });
    }
    let pub_of_internal = r.deltas(n, |x| x as u32);
    let ni = r.n();
    let internal_of_pub = r.deltas(ni, |x| x as u32);
    let eager_prologue = r.byte() != 0;
    let gen_start = r.n() as u32;
    let pub_lines = r.deltas(n, |x| x as i32);
    Translation { code, exception_table, extra_consts, pub_of_internal, internal_of_pub, eager_prologue, gen_start, pub_lines }
}
