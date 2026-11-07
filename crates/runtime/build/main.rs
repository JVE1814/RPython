//! (perf6) Build script of the runtime: writes the start-up image.
//!
//! The image is the analogue of HotSpot's class-data-sharing archive for the modules every
//! interpreter start runs: the frozen modules of `src/frozen.rs` (importlib's bootstrap,
//! os, site, ...), compiled by our own compiler crate and already translated to the
//! internal instruction form by the same translator the runtime uses on first execution
//! (`src/interp/translate.rs`, compiled into this script). At run time a frozen module is
//! then built straight from the image: no file access, no path search, no unmarshalling of
//! a pyc, no compilation and no translation. The format is described in
//! `src/startup_image/fmt.rs`.
//!
//! A module whose source needs the Unicode database to compile (non-ASCII identifiers,
//! `\N{...}` escapes) is left out; the runtime then compiles its frozen source as before.

#[allow(dead_code, unused_imports)]
#[path = "../src/interp/wc.rs"]
mod wc;

#[allow(dead_code, unused_imports, clippy::all)]
#[path = "../src/interp/translate.rs"]
mod translate;

#[path = "../src/startup_image/fmt.rs"]
mod fmt;

use fmt::*;
use rpy_compiler::wordcode::{CodeObject, Constant};
use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

thread_local! {
    /// Set when the parser asked one of the Unicode hooks something only the real
    /// database can answer.
    static NEEDS_UCD: Cell<bool> = const { Cell::new(false) };
}

fn taint() {
    NEEDS_UCD.with(|c| c.set(true));
}
fn h_lookup_name(_: &str) -> Option<u32> {
    taint();
    None
}
fn h_xid_start(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphabetic() || c == '_'
    } else {
        taint();
        false
    }
}
fn h_xid_continue(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphanumeric() || c == '_'
    } else {
        taint();
        false
    }
}
fn h_printable(c: char) -> bool {
    !c.is_control()
}
fn h_nfkc(s: &str) -> String {
    if !s.is_ascii() {
        taint();
    }
    s.to_string()
}

/// Paths (below Lib/) of the frozen modules' sources, in table order.
fn frozen_sources(frozen_rs: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = frozen_rs;
    while let Some(i) = rest.find("lib!(\"") {
        rest = &rest[i + 6..];
        let Some(j) = rest.find('"') else { break };
        let p = rest[..j].to_string();
        rest = &rest[j..];
        // the macro definition itself mentions `lib!(` only with a `$p` argument
        if !p.contains('$') && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

#[derive(Hash, PartialEq, Eq)]
enum StrKey {
    Utf8(String),
    Cps(Vec<u32>),
}

#[derive(Default)]
struct Image {
    strings: Vec<StrKey>,
    ids: HashMap<StrKey, u64>,
    code: Out,
}

impl Image {
    fn sid(&mut self, k: StrKey) -> u64 {
        if let Some(&i) = self.ids.get(&k) {
            return i;
        }
        let i = self.strings.len() as u64;
        let copy = match &k {
            StrKey::Utf8(s) => StrKey::Utf8(s.clone()),
            StrKey::Cps(c) => StrKey::Cps(c.clone()),
        };
        self.ids.insert(copy, i);
        self.strings.push(k);
        i
    }
    fn s(&mut self, s: &str) -> u64 {
        self.sid(StrKey::Utf8(s.to_string()))
    }
    fn cps(&mut self, cps: &[u32]) -> u64 {
        let as_chars: Option<String> = cps.iter().map(|&c| char::from_u32(c)).collect();
        match as_chars {
            Some(s) => self.sid(StrKey::Utf8(s)),
            None => self.sid(StrKey::Cps(cps.to_vec())),
        }
    }

    fn konst(&mut self, c: &Constant) {
        match c {
            Constant::None => self.code.byte(C_NONE),
            Constant::Bool(true) => self.code.byte(C_TRUE),
            Constant::Bool(false) => self.code.byte(C_FALSE),
            Constant::Ellipsis => self.code.byte(C_ELLIPSIS),
            Constant::Int(i) => {
                self.code.byte(C_INT);
                self.code.i(*i);
            }
            Constant::BigInt { neg, decimal } => {
                let id = self.s(decimal);
                self.code.byte(C_BIGINT);
                self.code.byte(*neg as u8);
                self.code.u(id);
            }
            Constant::Float(f) => {
                self.code.byte(C_FLOAT);
                self.code.raw(&f.to_bits().to_le_bytes());
            }
            Constant::Complex { real, imag } => {
                self.code.byte(C_COMPLEX);
                self.code.raw(&real.to_bits().to_le_bytes());
                self.code.raw(&imag.to_bits().to_le_bytes());
            }
            Constant::Str(cps) => {
                let id = self.cps(cps);
                self.code.byte(C_STR);
                self.code.u(id);
            }
            Constant::Bytes(b) => {
                self.code.byte(C_BYTES);
                self.code.blob(b);
            }
            Constant::Tuple(items) | Constant::FrozenSet(items) => {
                self.code.byte(if matches!(c, Constant::Tuple(_)) { C_TUPLE } else { C_FROZENSET });
                self.code.u(items.len() as u64);
                for it in items {
                    self.konst(it);
                }
            }
            Constant::Slice(parts) => {
                self.code.byte(C_SLICE);
                for it in parts.iter() {
                    self.konst(it);
                }
            }
            Constant::Code(co) => {
                self.code.byte(C_CODE);
                self.code_record(co);
            }
            Constant::Opaque(_) => panic!("opaque constant in a frozen module"),
        }
    }

    fn code_record(&mut self, co: &CodeObject) {
        let o = &mut self.code;
        for v in [co.argcount, co.posonlyargcount, co.kwonlyargcount, co.stacksize, co.flags, co.firstlineno] {
            o.u(v as u64);
        }
        o.blob(&co.code);
        o.u(co.consts.len() as u64);
        for c in &co.consts {
            self.konst(c);
        }
        let names: Vec<u64> = co.names.iter().map(|n| self.s(n)).collect();
        let lpn: Vec<u64> = co.localsplusnames.iter().map(|n| self.s(n)).collect();
        let name = self.s(&co.name);
        let qualname = self.s(&co.qualname);
        let o = &mut self.code;
        o.u(names.len() as u64);
        for n in names {
            o.u(n);
        }
        o.u(lpn.len() as u64);
        for n in lpn {
            o.u(n);
        }
        o.blob(&co.localspluskinds);
        o.u(name);
        o.u(qualname);
        o.blob(&co.linetable);
        o.blob(&co.exceptiontable);
        self.translation(co);
    }

    /// The internal form, exactly as `types::code::build_exec` would compute it.
    fn translation(&mut self, co: &CodeObject) {
        let ncu = co.code.len() / 2;
        let positions = wc::decode_linetable(&co.linetable, co.firstlineno.min(i32::MAX as u32), ncu);
        let inp = translate::Input {
            code: &co.code,
            exceptiontable: &co.exceptiontable,
            positions: &positions,
            nlocalsplus: co.localspluskinds.len(),
            nconsts: co.consts.len(),
            nnames: co.names.len(),
            stacksize: co.stacksize,
        };
        // a blob, so that the loader can skip it in one step (it is decoded on first
        // execution); empty = the translator failed, the runtime translates it itself
        let Ok(out) = translate::translate(&inp) else {
            self.code.blob(&[]);
            return;
        };
        let mut tx = Out::default();
        let o = &mut tx;
        o.u(out.code.len() as u64);
        for w in &out.code {
            o.raw(&w.to_le_bytes());
        }
        o.u(out.exception_table.len() as u64);
        for e in &out.exception_table {
            for v in [e.start, e.end, e.target, e.depth, e.lasti as u32] {
                o.u(v as u64);
            }
        }
        o.u(out.extra_consts.len() as u64);
        for x in &out.extra_consts {
            match x {
                translate::Extra::Int(i) => {
                    o.byte(X_INT);
                    o.i(*i);
                }
                translate::Extra::None => o.byte(X_NONE),
                translate::Extra::EmptyStr => o.byte(X_EMPTYSTR),
                translate::Extra::True => o.byte(X_TRUE),
                translate::Extra::False => o.byte(X_FALSE),
            }
        }
        assert_eq!(out.pub_of_internal.len(), out.code.len());
        put_deltas(o, out.pub_of_internal.iter().map(|&u| u as i64));
        o.u(out.internal_of_pub.len() as u64);
        put_deltas(o, out.internal_of_pub.iter().map(|&u| u as i64));
        o.byte(out.eager_prologue as u8);
        o.u(out.gen_start as u64);
        let lines = out.pub_of_internal.iter().map(|&u| positions.get(u as usize).and_then(|p| p.lineno).map_or(-1, |l| l as i64));
        put_deltas(o, lines);
        self.code.blob(&tx.buf);
    }
}

/// Compile one module the way the runtime compiles a frozen module (exec mode, no flags,
/// optimisation level 0). None if it needs the Unicode database or does not compile.
fn compile(src: &[u8], path: &str) -> Option<CodeObject> {
    let text = std::str::from_utf8(src).ok()?;
    let mut popts = rpy_parser::ParseOptions::new(rpy_parser::Mode::Exec);
    popts.hooks = rpy_parser::UnicodeHooks {
        lookup_name: h_lookup_name,
        is_xid_start: h_xid_start,
        is_xid_continue: h_xid_continue,
        is_printable: h_printable,
        nfkc: h_nfkc,
    };
    popts.filename = format!("<frozen {}>", path);
    popts.int_max_str_digits = 4300;
    NEEDS_UCD.with(|c| c.set(false));
    let parsed = rpy_parser::parse(text, &popts);
    let module = parsed.result.ok()?;
    if NEEDS_UCD.with(|c| c.get()) {
        return None;
    }
    let copts = rpy_compiler::CompileOptions::new(popts.filename.clone(), rpy_compiler::Mode::Exec);
    let out = rpy_compiler::compile(&module, &copts).ok()?;
    if NEEDS_UCD.with(|c| c.get()) {
        return None;
    }
    Some(out.code)
}

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let lib = root.join("../../Lib");
    let frozen_rs_path = root.join("src/frozen.rs");
    println!("cargo:rerun-if-changed={}", frozen_rs_path.display());
    println!("cargo:rerun-if-changed=build/main.rs");
    let frozen_rs = std::fs::read_to_string(&frozen_rs_path).expect("src/frozen.rs");
    let mut img = Image::default();
    let mut modules: Vec<(u64, u64)> = Vec::new();
    for p in frozen_sources(&frozen_rs) {
        let file = lib.join(&p);
        println!("cargo:rerun-if-changed={}", file.display());
        let Ok(src) = std::fs::read(&file) else { continue };
        let Some(co) = compile(&src, &p) else {
            println!("cargo:warning=startup image: {} is left out (compiled at run time)", p);
            continue;
        };
        let id = img.s(&p);
        modules.push((id, img.code.buf.len() as u64));
        img.code_record(&co);
    }
    // header: magic, string table, module table; code records follow
    let mut head = Out::default();
    head.raw(MAGIC);
    head.u(img.strings.len() as u64);
    for s in &img.strings {
        match s {
            StrKey::Utf8(s) => {
                head.byte(if s.is_ascii() { S_ASCII } else { S_UTF8 });
                head.blob(s.as_bytes());
            }
            StrKey::Cps(c) => {
                head.byte(S_CPS);
                head.u(c.len() as u64);
                for cp in c {
                    head.raw(&cp.to_le_bytes());
                }
            }
        }
    }
    // module offsets are relative to the image start: fixed-width (u32 LE) so the header
    // size does not depend on them
    head.u(modules.len() as u64);
    let table_at = head.buf.len();
    for _ in &modules {
        head.raw(&[0; 12]);
    }
    let base = head.buf.len() as u64;
    for (k, (id, off)) in modules.iter().enumerate() {
        let at = table_at + 12 * k;
        head.buf[at..at + 4].copy_from_slice(&(*id as u32).to_le_bytes());
        head.buf[at + 4..at + 12].copy_from_slice(&(base + off).to_le_bytes());
    }
    head.raw(&img.code.buf);
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("startup_image.bin");
    std::fs::write(out, &head.buf).expect("write startup image");
}
