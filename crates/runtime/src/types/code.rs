//! Code objects.
//!
//! The struct starts with CPython's public `PyCodeObject` prefix (fields extensions may
//! read). Two Rust-owned parts follow:
//! * `CodeMeta` — always present: the public form (co_code wordcode, consts, names,
//!   localsplus) as built objects;
//! * `CodeData` — the internal executable form (our 32-bit instruction stream, per-word
//!   locations, decoded exception table, inline caches, tiering counters, and the maps
//!   between internal pcs and public code-unit offsets). For code objects in the public
//!   (CPython 3.15 wordcode) format it is produced by `interp::translate` on first use
//!   and published once with release/acquire; legacy code objects (our compiler's old
//!   output, D24 transition) carry it from construction.

use crate::errors::*;
use crate::ffi::*;
use crate::interp::wc;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bytes, int, none, str as pystr, tuple};
use rpy_compiler::bytecode::{self as bc, ExceptionEntry, Location};
use std::ffi::c_int;
use std::sync::atomic::{AtomicPtr, Ordering};

#[repr(C)]
pub struct PyCodeObject {
    pub ob_base: PyVarObject,
    pub co_consts: *mut PyObject,
    pub co_names: *mut PyObject,
    pub co_exceptiontable: *mut PyObject,
    pub co_flags: c_int,
    pub co_argcount: c_int,
    pub co_posonlyargcount: c_int,
    pub co_kwonlyargcount: c_int,
    pub co_stacksize: c_int,
    pub co_firstlineno: c_int,
    pub co_nlocalsplus: c_int,
    pub co_framesize: c_int,
    pub co_nlocals: c_int,
    pub co_ncellvars: c_int,
    pub co_nfreevars: c_int,
    pub co_version: u32,
    pub co_localsplusnames: *mut PyObject,
    pub co_localspluskinds: *mut PyObject,
    pub co_filename: *mut PyObject,
    pub co_name: *mut PyObject,
    pub co_qualname: *mut PyObject,
    pub co_linetable: *mut PyObject,
    pub co_weakreflist: *mut PyObject,
    pub _reserved: [usize; 8],
    /// Internal executable form (owned Box, NULL until translated).
    pub data: AtomicPtr<CodeData>,
    /// Public form (owned Box, never NULL).
    pub meta: *mut CodeMeta,
}

/// Public form of a code object (as objects), always present.
pub struct CodeMeta {
    /// co_code is CPython 3.15 wordcode (false: legacy internal words, D24 transition)
    pub public: bool,
    /// the co_code bytes object
    pub co_code: PyRef,
    pub consts: Vec<PyRef>,
    /// interned str objects
    pub names: Vec<PyRef>,
    pub localsplusnames: Vec<PyRef>,
    pub localspluskinds: Vec<u8>,
    pub co_name: PyRef,
    pub co_qualname: PyRef,
    pub filename: PyRef,
    /// Indices of cell variables in localsplus.
    pub cell_slots: Vec<u32>,
}

/// Executable representation.
pub struct CodeData {
    pub code: Vec<u32>,
    pub locations: Vec<Location>,
    pub exception_table: Vec<ExceptionEntry>,
    /// Constants as objects: co_consts followed by the translator's extra constants.
    pub consts: Vec<PyRef>,
    /// Names (interned str objects).
    pub names: Vec<PyRef>,
    pub localsplusnames: Vec<PyRef>,
    pub localspluskinds: Vec<u8>,
    pub co_name: PyRef,
    pub co_qualname: PyRef,
    pub filename: PyRef,
    /// Indices of cell variables in localsplus.
    pub cell_slots: Vec<u32>,
    /// HotSpot-style counters for tiering.
    pub invocation_count: std::sync::atomic::AtomicU32,
    pub backedge_count: std::sync::atomic::AtomicU32,
    /// Per-instruction inline caches (owned by the interpreter).
    pub caches: crate::interp::cache::CodeCaches,
    /// Per internal word: public code-unit offset of the instruction it implements.
    pub pub_of_internal: Vec<u32>,
    /// Per public code unit (+1): first internal word of the containing instruction.
    pub internal_of_pub: Vec<u32>,
    /// A generator prologue must run eagerly up to the internal `ReturnGenerator`.
    pub eager_prologue: bool,
    /// Internal pc where a generator body starts (after RETURN_GENERATOR).
    pub gen_start: u32,
    /// Translation failed: executing the code raises SystemError(msg).
    pub invalid: Option<String>,
    /// Public objects: line of each internal word's public instruction (-1 = none);
    /// empty for legacy objects.
    pub pub_lines: Vec<i32>,
    /// Public objects: observed CPython specialisation per internal word (interp::adaptive).
    pub spec: Box<[std::sync::atomic::AtomicU8]>,
    /// (core6) The same view for the other threads (thread-local bytecode, `-X tlbc`).
    pub spec_tl: crate::interp::adaptive::ThreadViews,
}

impl CodeData {
    /// Public code-unit offset of internal pc `i`.
    #[inline]
    pub fn pub_offset(&self, i: usize) -> u32 {
        match self.pub_of_internal.get(i) {
            Some(&o) => o,
            None => self.pub_of_internal.last().map_or(0, |&o| o),
        }
    }
    /// First internal pc of the public instruction containing code unit `cu`.
    pub fn internal_pc(&self, cu: usize) -> u32 {
        match self.internal_of_pub.get(cu) {
            Some(&i) => i,
            None => self.code.len() as u32,
        }
    }
    /// Is internal pc `i` the first word of its public instruction?
    #[inline]
    pub fn starts_public_instr(&self, i: usize) -> bool {
        i == 0 || self.pub_of_internal.get(i) != self.pub_of_internal.get(i - 1)
    }
}

/// Convenience accessor bundle.
pub struct CodeRef<'a> {
    pub obj: &'a PyCodeObject,
    pub d: &'a CodeMeta,
    pub co_name: &'a PyRef,
    pub co_qualname: &'a PyRef,
}

impl<'a> CodeRef<'a> {
    pub fn nfreevars(&self) -> usize {
        self.obj.co_nfreevars as usize
    }
    pub fn flags(&self) -> u32 {
        self.obj.co_flags as u32
    }
    pub fn docstring(&self) -> Option<PyRef> {
        if self.flags() & bc::co::HAS_DOCSTRING != 0 {
            if let Some(c) = self.d.consts.first() {
                if pystr::is_str(c) {
                    return Some(c.clone());
                }
            }
        }
        None
    }
}

#[unsafe(no_mangle)]
pub static mut PyCode_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn code_type() -> *mut PyTypeObject {
    &raw mut PyCode_Type
}
pub fn is_code(o: &PyObject) -> bool {
    o.is_type(code_type())
}

#[inline(always)]
fn get(o: &PyObject) -> &PyCodeObject {
    unsafe { &*(o.as_ptr() as *const PyCodeObject) }
}

#[inline(always)]
pub fn code_meta(o: &PyObject) -> &CodeMeta {
    unsafe { &*get(o).meta }
}

pub fn code(o: &PyObject) -> CodeRef<'_> {
    let c = get(o);
    let d = unsafe { &*c.meta };
    CodeRef { obj: c, d, co_name: &d.co_name, co_qualname: &d.co_qualname }
}

/// The executable form (translated on first use).
#[inline(always)]
pub fn code_data(o: &PyObject) -> &CodeData {
    let p = get(o).data.load(Ordering::Acquire);
    if !p.is_null() {
        return unsafe { &*p };
    }
    code_data_slow(o)
}

#[cold]
fn code_data_slow(o: &PyObject) -> &CodeData {
    let c = get(o);
    let built = build_exec(c, unsafe { &*c.meta });
    if std::env::var_os("RPYTHON_TRANSLATE_DEBUG").is_some() {
        let m = unsafe { &*c.meta };
        eprintln!("== translate {} ({})", pystr::to_string_lossy(&m.co_qualname), pystr::to_string_lossy(&m.filename));
        if let Some(e) = &built.invalid {
            eprintln!("   ERROR {}", e);
        }
        for (i, w) in built.code.iter().enumerate() {
            let (op, a) = bc::decode(*w);
            eprintln!("   {:4} [pub {:4}] {:?} {}", i, built.pub_offset(i) * 2, op, a);
        }
        for e in &built.exception_table {
            eprintln!("   exc {}..{} -> {} depth {} lasti {}", e.start, e.end, e.target, e.depth, e.lasti);
        }
    }
    let fresh = Box::into_raw(Box::new(built));
    match c.data.compare_exchange(std::ptr::null_mut(), fresh, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => unsafe { &*fresh },
        Err(winner) => {
            drop(unsafe { Box::from_raw(fresh) });
            unsafe { &*winner }
        }
    }
}

/// Has the code object been translated (or was it built in the internal form)?
pub fn is_translated(o: &PyObject) -> bool {
    !get(o).data.load(Ordering::Acquire).is_null()
}

/// Public code units of the code object.
pub fn ncodeunits(o: &PyObject) -> usize {
    bytes::bytes_data(&code_meta(o).co_code).len() / 2
}

/// Per-code-unit positions decoded from co_linetable (public objects).
pub fn positions(o: &PyObject) -> Vec<wc::Position> {
    let c = get(o);
    let lt = bytes::bytes_data(unsafe { &*c.co_linetable });
    wc::decode_linetable(lt, c.co_firstlineno.max(0) as u32, ncodeunits(o))
}

fn exec_extra_obj(e: &crate::interp::translate::Extra) -> PyRef {
    use crate::interp::translate::Extra;
    match e {
        Extra::Int(i) => int::new_int(*i),
        Extra::None => none::none_ref(),
        Extra::EmptyStr => pystr::intern_str(""),
        Extra::True => crate::types::bool_::new_bool(true),
        Extra::False => crate::types::bool_::new_bool(false),
    }
}

/// Translate the public form into the executable form (never fails: invalid code gets a
/// body that raises SystemError when run).
fn build_exec(c: &PyCodeObject, m: &CodeMeta) -> CodeData {
    let code = bytes::bytes_data(&m.co_code);
    let lt = bytes::bytes_data(unsafe { &*c.co_linetable });
    let et = bytes::bytes_data(unsafe { &*c.co_exceptiontable });
    let ncu = code.len() / 2;
    let positions = wc::decode_linetable(lt, c.co_firstlineno.max(0) as u32, ncu);
    let inp = crate::interp::translate::Input {
        code,
        exceptiontable: et,
        positions: &positions,
        nlocalsplus: m.localspluskinds.len(),
        nconsts: m.consts.len(),
        nnames: m.names.len(),
        stacksize: c.co_stacksize.max(0) as u32,
    };
    let base = |code: Vec<u32>, locations: Vec<Location>, consts: Vec<PyRef>| CodeData {
        caches: crate::interp::cache::CodeCaches::new(code.len()),
        code,
        locations,
        exception_table: Vec::new(),
        consts,
        names: m.names.clone(),
        localsplusnames: m.localsplusnames.clone(),
        localspluskinds: m.localspluskinds.clone(),
        co_name: m.co_name.clone(),
        co_qualname: m.co_qualname.clone(),
        filename: m.filename.clone(),
        cell_slots: m.cell_slots.clone(),
        invocation_count: std::sync::atomic::AtomicU32::new(0),
        backedge_count: std::sync::atomic::AtomicU32::new(0),
        pub_of_internal: Vec::new(),
        internal_of_pub: Vec::new(),
        eager_prologue: false,
        gen_start: 0,
        invalid: None,
        pub_lines: Vec::new(),
        spec: Box::new([]),
        spec_tl: Default::default(),
    };
    match crate::interp::translate::translate(&inp) {
        Ok(out) => {
            let mut consts = m.consts.clone();
            consts.extend(out.extra_consts.iter().map(exec_extra_obj));
            // (perf4) no per-instruction Location table for public code: lines come from
            // `pub_lines` (16 bytes per internal instruction saved)
            drop(out.locations);
            let mut d = base(out.code, Vec::new(), consts);
            d.exception_table = out.exception_table;
            d.pub_of_internal = out.pub_of_internal;
            d.internal_of_pub = out.internal_of_pub;
            d.eager_prologue = out.eager_prologue;
            d.gen_start = out.gen_start;
            d.pub_lines = d.pub_of_internal.iter().map(|&u| positions.get(u as usize).and_then(|p| p.lineno).map_or(-1, |l| l as i32)).collect();
            d.spec = (0..d.code.len()).map(|_| std::sync::atomic::AtomicU8::new(0)).collect();
            d
        }
        Err(msg) => {
            let line = c.co_firstlineno.max(0) as u32;
            let loc = Location { lineno: line, end_lineno: line, col: u32::MAX, end_col: u32::MAX };
            let mut d = base(vec![bc::encode(bc::Op::Unused1, 0)], vec![loc], m.consts.clone());
            d.pub_of_internal = vec![0];
            d.internal_of_pub = vec![0; ncu + 1];
            d.invalid = Some(msg);
            d
        }
    }
}

/// Constant tuples made only of simple immutable constants are shared between all code
/// objects (like CPython's interned constants): key = a type-tagged rendering.
static CONST_TUPLES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, PyRef>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Type-tagged rendering of a constant (equal keys = same type and value, so the objects
/// are interchangeable); None for constants that are never merged: code objects, and NaNs
/// (two NaN objects are distinct set members and unequal dict keys, so a constant
/// `{nan1, nan2}` must keep both).
fn const_key(c: &rpy_compiler::wordcode::Constant) -> Option<String> {
    use rpy_compiler::wordcode::Constant as C;
    use std::fmt::Write;
    fn go(c: &C, out: &mut String, depth: usize) -> bool {
        if depth > 16 || out.len() > 1 << 16 {
            return false;
        }
        match c {
            C::None => out.push('N'),
            C::Ellipsis => out.push('E'),
            C::Bool(b) => out.push(if *b { 'T' } else { 'F' }),
            C::Int(i) => {
                let _ = write!(out, "i{};", i);
            }
            C::BigInt { neg, decimal } => {
                let _ = write!(out, "I{}{};", if *neg { "-" } else { "" }, decimal);
            }
            C::Float(f) => {
                if f.is_nan() {
                    return false;
                }
                let _ = write!(out, "f{:x};", f.to_bits());
            }
            C::Complex { real, imag } => {
                if real.is_nan() || imag.is_nan() {
                    return false;
                }
                let _ = write!(out, "c{:x},{:x};", real.to_bits(), imag.to_bits());
            }
            C::Str(cps) => {
                let _ = write!(out, "s{}:", cps.len());
                for cp in cps {
                    let _ = write!(out, "{:x},", cp);
                }
            }
            C::Bytes(b) => {
                let _ = write!(out, "b{}:", b.len());
                for x in b {
                    let _ = write!(out, "{:02x}", x);
                }
            }
            C::Tuple(items) => {
                let _ = write!(out, "({}:", items.len());
                for it in items {
                    if !go(it, out, depth + 1) {
                        return false;
                    }
                }
                out.push(')');
            }
            C::FrozenSet(items) => {
                let mut keys = Vec::with_capacity(items.len());
                for it in items {
                    let mut k = String::new();
                    if !go(it, &mut k, depth + 1) {
                        return false;
                    }
                    keys.push(k);
                }
                keys.sort();
                let _ = write!(out, "{{{}:", keys.len());
                for k in keys {
                    out.push_str(&k);
                    out.push('|');
                }
                out.push('}');
            }
            C::Slice(parts) => {
                out.push('[');
                for it in parts.iter() {
                    if !go(it, out, depth + 1) {
                        return false;
                    }
                }
                out.push(']');
            }
            C::Code(_) | C::Opaque(_) => return false,
        }
        true
    }
    let mut s = String::new();
    if go(c, &mut s, 0) { Some(s) } else { None }
}

/// Per-compilation sharing (one `compile()` call): equal constants, equal co_consts
/// tuples, equal name tuples and equal line/exception tables become one object.
#[derive(Default)]
pub struct ConstMerger {
    consts: std::collections::HashMap<String, PyRef>,
    blobs: std::collections::HashMap<Vec<u8>, PyRef>,
}

impl ConstMerger {
    fn bytes(&mut self, b: &[u8]) -> PyRef {
        if let Some(s) = bytes::bytes_singleton(b) {
            return s;
        }
        self.blobs.entry(b.to_vec()).or_insert_with(|| bytes::new_bytes(b)).clone()
    }
}

/// Convert a public-form compiler constant into an object.
pub fn wconst_to_obj(c: &rpy_compiler::wordcode::Constant, filename: &PyRef) -> PyResult {
    wconst_merged(c, filename, &mut ConstMerger::default())
}

fn wconst_merged(c: &rpy_compiler::wordcode::Constant, filename: &PyRef, m: &mut ConstMerger) -> PyResult {
    use rpy_compiler::wordcode::Constant as C;
    let key = const_key(c);
    if let Some(k) = &key {
        if let Some(v) = m.consts.get(k) {
            return Ok(v.clone());
        }
    }
    let v = match c {
        C::None => none::none_ref(),
        C::Bool(b) => crate::types::bool_::new_bool(*b),
        C::Ellipsis => none::ellipsis().to_ref(),
        C::Int(i) => int::new_int(*i),
        C::BigInt { neg, decimal } => crate::types::int_methods::from_decimal_str(*neg, decimal)?,
        C::Float(f) => crate::types::float::new_float(*f),
        C::Complex { real, imag } => crate::types::complex::new_complex(*real, *imag),
        C::Str(cps) => pystr::intern(pystr::from_codepoints(cps)),
        C::Bytes(b) => m.bytes(b),
        C::Tuple(items) => {
            let global = key.as_ref().and_then(|k| CONST_TUPLES.lock().unwrap().get(k).cloned());
            match global {
                Some(t) => t,
                None => {
                    let mut v = Vec::with_capacity(items.len());
                    for it in items {
                        v.push(wconst_merged(it, filename, m)?);
                    }
                    let t = tuple::new_tuple(v);
                    match &key {
                        Some(k) => CONST_TUPLES.lock().unwrap().entry(k.clone()).or_insert(t).clone(),
                        None => t,
                    }
                }
            }
        }
        C::FrozenSet(items) => {
            let mut v = Vec::with_capacity(items.len());
            for it in items {
                v.push(wconst_merged(it, filename, m)?);
            }
            crate::types::set::new_frozenset_from_vec(v)?
        }
        C::Slice(parts) => {
            let a = wconst_merged(&parts[0], filename, m)?;
            let b = wconst_merged(&parts[1], filename, m)?;
            let s = wconst_merged(&parts[2], filename, m)?;
            crate::types::slice::new_slice(&a, &b, &s)
        }
        C::Code(co) => new_code_wc_merged(co, filename, m)?,
        // placeholders of the compiler test hooks; they substitute the real objects
        C::Opaque(_) => none::none_ref(),
    };
    if let Some(k) = key {
        m.consts.insert(k, v.clone());
    }
    Ok(v)
}

/// Build a (public-form) code object from the compiler's wordcode output.
pub fn new_code_from_wordcode(co: &rpy_compiler::wordcode::CodeObject, filename: Option<&PyRef>) -> PyResult {
    let filename = match filename {
        Some(f) => f.clone(),
        None => pystr::intern_str(&co.filename),
    };
    new_code_wc_merged(co, &filename, &mut ConstMerger::default())
}

fn new_code_wc_merged(co: &rpy_compiler::wordcode::CodeObject, filename: &PyRef, m: &mut ConstMerger) -> PyResult {
    let mut consts = Vec::with_capacity(co.consts.len());
    let mut tuple_key = Some(String::from("C"));
    for c in &co.consts {
        match (const_key(c), &mut tuple_key) {
            (Some(k), Some(tk)) => {
                tk.push_str(&k);
                tk.push('\u{1}');
            }
            _ => tuple_key = None,
        }
        consts.push(wconst_merged(c, filename, m)?);
    }
    let r = new_code_from_public(PublicParts {
        argcount: co.argcount as i32,
        posonlyargcount: co.posonlyargcount as i32,
        kwonlyargcount: co.kwonlyargcount as i32,
        stacksize: co.stacksize as i32,
        flags: co.flags as i32,
        firstlineno: co.firstlineno as i32,
        code: bytes::new_bytes(&co.code),
        consts,
        names: co.names.iter().map(|n| pystr::intern_str(n)).collect(),
        localsplusnames: co.localsplusnames.iter().map(|n| pystr::intern_str(n)).collect(),
        kinds: co.localspluskinds.clone(),
        filename: filename.clone(),
        name: pystr::intern_str(&co.name),
        qualname: pystr::intern_str(&co.qualname),
        linetable: m.bytes(&co.linetable),
        exceptiontable: m.bytes(&co.exceptiontable),
    })?;
    // equal co_consts tuples (same items, all merged above) are shared too
    if let Some(k) = tuple_key {
        let c = unsafe { &mut *(r.as_ptr() as *mut PyCodeObject) };
        let mine = unsafe { PyRef::from_borrowed(c.co_consts) };
        let shared = m.consts.entry(k).or_insert(mine).clone();
        if !shared.is(unsafe { &*c.co_consts }) {
            let old = std::mem::replace(&mut c.co_consts, shared.into_raw());
            unsafe { xdecref(old) };
        }
    }
    Ok(r)
}

/// Everything a legacy code object is made of, with the object-valued parts already built
/// (used by the compiler path above and by marshal, which reads objects directly).
pub struct CodeParts {
    pub argcount: u32,
    pub posonlyargcount: u32,
    pub kwonlyargcount: u32,
    pub flags: u32,
    pub stacksize: u32,
    pub firstlineno: u32,
    pub code: Vec<u32>,
    pub locations: Vec<Location>,
    pub exception_table: Vec<ExceptionEntry>,
    pub kinds: Vec<u8>,
    pub filename: PyRef,
    pub consts: Vec<PyRef>,
    /// interned names
    pub names: Vec<PyRef>,
    pub localsplusnames: Vec<PyRef>,
    pub name: PyRef,
    pub qualname: PyRef,
}

/// Public-form parts (CPython 3.15 layout); validated by `new_code_from_public`.
pub struct PublicParts {
    pub argcount: i32,
    pub posonlyargcount: i32,
    pub kwonlyargcount: i32,
    pub stacksize: i32,
    pub flags: i32,
    pub firstlineno: i32,
    /// co_code bytes object (wordcode)
    pub code: PyRef,
    pub consts: Vec<PyRef>,
    pub names: Vec<PyRef>,
    pub localsplusnames: Vec<PyRef>,
    pub kinds: Vec<u8>,
    pub filename: PyRef,
    pub name: PyRef,
    pub qualname: PyRef,
    /// bytes objects
    pub linetable: PyRef,
    pub exceptiontable: PyRef,
}

fn cell_slots_of(kinds: &[u8]) -> Vec<u32> {
    kinds.iter().enumerate().filter(|(_, k)| **k & bc::fast::CELL != 0).map(|(i, _)| i as u32).collect()
}

/// Allocate the object and fill the public prefix.
#[allow(clippy::too_many_arguments)]
unsafe fn alloc_code(
    argcount: i32, posonly: i32, kwonly: i32, stacksize: i32, flags: i32, firstlineno: i32, meta: CodeMeta,
    consts: PyRef, names: PyRef, exctable: PyRef, linetable: PyRef,
) -> PyRef {
    unsafe {
        let kinds = &meta.localspluskinds;
        let nlocals = kinds.iter().filter(|&&k| k & bc::fast::LOCAL != 0).count();
        let ncell = kinds.iter().filter(|&&k| k & bc::fast::CELL != 0).count();
        let nfree = kinds.iter().filter(|&&k| k & bc::fast::FREE != 0).count();
        let op = crate::alloc::alloc_object(code_type(), std::mem::size_of::<PyCodeObject>());
        let c = &mut *(op as *mut PyCodeObject);
        c.co_consts = consts.into_raw();
        c.co_names = names.into_raw();
        c.co_exceptiontable = exctable.into_raw();
        c.co_flags = flags;
        c.co_argcount = argcount;
        c.co_posonlyargcount = posonly;
        c.co_kwonlyargcount = kwonly;
        c.co_stacksize = stacksize;
        c.co_firstlineno = firstlineno;
        c.co_nlocalsplus = kinds.len() as c_int;
        c.co_framesize = (kinds.len() + stacksize.max(0) as usize) as c_int;
        c.co_nlocals = nlocals as c_int;
        c.co_ncellvars = ncell as c_int;
        c.co_nfreevars = nfree as c_int;
        c.co_version = 1;
        c.co_localsplusnames = tuple::new_tuple(meta.localsplusnames.clone()).into_raw();
        c.co_localspluskinds = bytes::new_bytes(kinds).into_raw();
        c.co_filename = meta.filename.clone().into_raw();
        c.co_name = meta.co_name.clone().into_raw();
        c.co_qualname = meta.co_qualname.clone().into_raw();
        c.co_linetable = linetable.into_raw();
        std::ptr::write(&mut c.data, AtomicPtr::new(std::ptr::null_mut()));
        c.meta = Box::into_raw(Box::new(meta));
        PyRef::from_raw(op)
    }
}

pub fn new_code_from_parts(p: CodeParts) -> PyResult {
    let kinds = p.kinds;
    let cell_slots = cell_slots_of(&kinds);
    let exctable = encode_exception_table(&p.exception_table);
    let linetable = encode_locations(&p.locations);
    let ninstr = p.code.len();
    let mut cob = Vec::with_capacity(p.code.len() * 4);
    for w in &p.code {
        cob.extend_from_slice(&w.to_le_bytes());
    }
    let meta = CodeMeta {
        public: false,
        co_code: bytes::new_bytes(&cob),
        consts: p.consts.clone(),
        names: p.names.clone(),
        localsplusnames: p.localsplusnames.clone(),
        localspluskinds: kinds.clone(),
        co_name: p.name.clone(),
        co_qualname: p.qualname.clone(),
        filename: p.filename.clone(),
        cell_slots: cell_slots.clone(),
    };
    let data = Box::new(CodeData {
        code: p.code,
        locations: p.locations,
        exception_table: p.exception_table,
        consts: p.consts.clone(),
        names: p.names.clone(),
        localsplusnames: p.localsplusnames.clone(),
        localspluskinds: kinds.clone(),
        co_name: p.name.clone(),
        co_qualname: p.qualname.clone(),
        filename: p.filename.clone(),
        cell_slots,
        invocation_count: std::sync::atomic::AtomicU32::new(0),
        backedge_count: std::sync::atomic::AtomicU32::new(0),
        caches: crate::interp::cache::CodeCaches::new(ninstr),
        // legacy words are 4 bytes = 2 "code units"
        pub_of_internal: (0..ninstr as u32).map(|i| 2 * i).collect(),
        internal_of_pub: (0..=2 * ninstr as u32).map(|u| u / 2).collect(),
        eager_prologue: false,
        gen_start: 0,
        invalid: None,
        pub_lines: Vec::new(),
        spec: Box::new([]),
        spec_tl: Default::default(),
    });
    unsafe {
        let r = alloc_code(
            p.argcount as i32,
            p.posonlyargcount as i32,
            p.kwonlyargcount as i32,
            p.stacksize as i32,
            p.flags as i32,
            p.firstlineno as i32,
            meta,
            tuple::new_tuple(p.consts),
            tuple::new_tuple(p.names),
            bytes::new_bytes(&exctable),
            bytes::new_bytes(&linetable),
        );
        get(&r).data.store(Box::into_raw(data), Ordering::Release);
        Ok(r)
    }
}

/// Build a public-form code object (validating like `types.CodeType`). The internal form
/// is created lazily by `code_data`.
pub fn new_code_from_public(p: PublicParts) -> PyResult {
    let bad = || -> PyErr { system_error("bad argument to internal function") };
    if p.argcount < p.posonlyargcount || p.posonlyargcount < 0 || p.kwonlyargcount < 0 || p.stacksize < 0 || p.flags < 0 {
        return Err(bad());
    }
    if !bytes::is_bytes(&p.code) || !bytes::is_bytes(&p.linetable) || !bytes::is_bytes(&p.exceptiontable) {
        return Err(bad());
    }
    if p.localsplusnames.len() != p.kinds.len() {
        return Err(bad());
    }
    if !pystr::is_str(&p.name) || !pystr::is_str(&p.qualname) || !pystr::is_str(&p.filename) {
        return Err(bad());
    }
    if p.names.iter().chain(p.localsplusnames.iter()).any(|n| !pystr::is_str(n)) {
        return Err(bad());
    }
    let cl = bytes::bytes_data(&p.code).len();
    if cl % 2 != 0 || cl == 0 {
        raise!(ValueError, "code: co_code is malformed");
    }
    let nargs = p.argcount as usize
        + p.kwonlyargcount as usize
        + (p.flags as u32 & bc::co::VARARGS != 0) as usize
        + (p.flags as u32 & bc::co::VARKEYWORDS != 0) as usize;
    let nlocals = p.kinds.iter().filter(|&&k| k & bc::fast::LOCAL != 0).count();
    if nargs > nlocals {
        raise!(ValueError, "code: co_varnames is too small");
    }
    let intern = |v: Vec<PyRef>| -> Vec<PyRef> {
        v.into_iter().map(|n| if pystr::is_str_exact(&n) { pystr::intern(n) } else { n }).collect()
    };
    let names = intern(p.names);
    let lpn = intern(p.localsplusnames);
    let meta = CodeMeta {
        public: true,
        co_code: p.code,
        consts: p.consts.clone(),
        names: names.clone(),
        localsplusnames: lpn,
        cell_slots: cell_slots_of(&p.kinds),
        localspluskinds: p.kinds,
        co_name: if pystr::is_str_exact(&p.name) { pystr::intern(p.name) } else { p.name },
        co_qualname: p.qualname,
        filename: p.filename,
    };
    let r: PyResult = unsafe {
        Ok(alloc_code(
            p.argcount,
            p.posonlyargcount,
            p.kwonlyargcount,
            p.stacksize,
            p.flags,
            p.firstlineno,
            meta,
            tuple::new_tuple(p.consts),
            tuple::new_tuple(names),
            p.exceptiontable,
            p.linetable,
        ))
    };
    r.inspect(|r| {
        // RPYTHON_TRANSLATE_CHECK=1: translate eagerly and report failures (translator tests)
        static CHECK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *CHECK.get_or_init(|| std::env::var_os("RPYTHON_TRANSLATE_CHECK").is_some()) {
            if let Some(e) = &code_data(r).invalid {
                let m = code_meta(r);
                eprintln!("TRANSLATE-ERROR {}:{} {}: {}", pystr::to_string_lossy(&m.filename), get(r).co_firstlineno, pystr::to_string_lossy(&m.co_qualname), e);
            }
        }
    })
}

fn encode_exception_table(t: &[ExceptionEntry]) -> Vec<u8> {
    let mut v = Vec::with_capacity(t.len() * 16);
    for e in t {
        for x in [e.start, e.end, e.target, e.depth, e.lasti as u32] {
            v.extend_from_slice(&x.to_le_bytes());
        }
    }
    v
}

/// Legacy linetable encoding: per instruction (lineno, end_lineno, col, end_col) as u32 LE.
fn encode_locations(locs: &[Location]) -> Vec<u8> {
    let mut v = Vec::with_capacity(locs.len() * 16);
    for l in locs {
        for x in [l.lineno, l.end_lineno, l.col, l.end_col] {
            v.extend_from_slice(&x.to_le_bytes());
        }
    }
    v
}

/// Line number for an internal instruction index (walking back over artificial ones).
pub fn line_for_instr(co: &PyObject, i: usize) -> Option<u32> {
    let d = code_data(co);
    if d.locations.is_empty() && !d.pub_lines.is_empty() {
        // public code keeps only the line per instruction (`pub_lines`, -1 = none)
        let mut j = i.min(d.pub_lines.len() - 1);
        loop {
            let l = d.pub_lines[j];
            if l > 0 {
                return Some(l as u32);
            }
            if j == 0 {
                return None;
            }
            j -= 1;
        }
    }
    let mut j = i.min(d.locations.len().saturating_sub(1));
    loop {
        if let Some(l) = d.locations.get(j) {
            if l.lineno != 0 {
                return Some(l.lineno);
            }
        }
        if j == 0 {
            return None;
        }
        j -= 1;
    }
}

pub fn code_name(o: &PyObject) -> PyRef {
    code_meta(o).co_name.clone()
}
pub fn code_qualname(o: &PyObject) -> PyRef {
    code_meta(o).co_qualname.clone()
}
pub fn code_docstring(o: &PyObject) -> Option<PyRef> {
    code(o).docstring()
}

/// Line of internal pc `i` as frames report it (f_lineno, tracebacks): for public code
/// exactly the line co_lines() gives the instruction (None for artificial ones); legacy
/// code walks back over artificial instructions.
pub fn frame_line(co: &PyObject, i: usize) -> Option<u32> {
    let d = code_data(co);
    if !d.pub_lines.is_empty() {
        return d.pub_lines.get(i.min(d.pub_lines.len() - 1)).and_then(|&l| if l < 0 { None } else { Some(l as u32) });
    }
    line_for_instr(co, i)
}

/// f_lasti / tb_lasti value (bytes) for internal pc `i`.
pub fn lasti_bytes(co: &PyObject, i: usize) -> i64 {
    code_data(co).pub_offset(i) as i64 * 2
}

/// Line of the public instruction at byte offset `addr` (PyCode_Addr2Line semantics).
pub fn addr2line(co: &PyObject, addr: i64) -> i32 {
    if addr < 0 {
        return get(co).co_firstlineno;
    }
    let cu = (addr / 2) as usize;
    positions(co).get(cu).and_then(|p| p.lineno).map_or(-1, |l| l as i32)
}

/// (line, col, end_line, end_col) of the instruction at byte offset `addr`; -1 = unknown.
pub fn addr2location(co: &PyObject, addr: i64) -> (i32, i32, i32, i32) {
    if addr < 0 {
        let l = get(co).co_firstlineno;
        return (l, -1, l, -1);
    }
    let o = |x: Option<u32>| x.map_or(-1, |v| v as i32);
    match positions(co).get((addr / 2) as usize) {
        Some(p) => (o(p.lineno), o(p.col), o(p.end_lineno), o(p.end_col)),
        None => (-1, -1, -1, -1),
    }
}

unsafe extern "C" fn code_dealloc(op: *mut PyObject) {
    unsafe {
        let c = &mut *(op as *mut PyCodeObject);
        crate::types::weakref::clear_weakrefs_at(op, &mut c.co_weakreflist);
        crate::capi::funcs::free_code_extras(op);
        for p in [
            c.co_consts,
            c.co_names,
            c.co_exceptiontable,
            c.co_localsplusnames,
            c.co_localspluskinds,
            c.co_filename,
            c.co_name,
            c.co_qualname,
            c.co_linetable,
        ] {
            xdecref(p);
        }
        let d = c.data.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if !d.is_null() {
            drop(Box::from_raw(d));
        }
        if !c.meta.is_null() {
            drop(Box::from_raw(c.meta));
            c.meta = std::ptr::null_mut();
        }
        crate::alloc::free_object(op);
    }
}

unsafe extern "C" fn code_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let c = &*(op as *mut PyCodeObject);
        let s = format!(
            "<code object {} at {:p}, file \"{}\", line {}>",
            pystr::to_string_lossy(&*c.co_name),
            op,
            pystr::to_string_lossy(&*c.co_filename),
            c.co_firstlineno
        );
        pystr::new_str(&s).into_raw()
    }
}

/// Constant equality for code comparison: same type, floats/complex compared by bits
/// (0.0 vs -0.0, NaN), containers element-wise with the same rule.
fn const_eq(p: &PyObject, q: &PyObject) -> PyResult<bool> {
    if p.is(q) {
        return Ok(true);
    }
    if p.type_ptr() != q.type_ptr() {
        return Ok(false);
    }
    if crate::types::float::is_float(p) {
        return Ok(crate::types::float::float_value(p).to_bits() == crate::types::float::float_value(q).to_bits());
    }
    if crate::types::complex::is_complex(p) {
        let (a, b) = (crate::types::complex::complex_value(p), crate::types::complex::complex_value(q));
        return Ok(a.real.to_bits() == b.real.to_bits() && a.imag.to_bits() == b.imag.to_bits());
    }
    if tuple::is_tuple_exact(p) {
        let (a, b) = (tuple::tuple_items(p), tuple::tuple_items(q));
        if a.len() != b.len() {
            return Ok(false);
        }
        for (x, y) in a.iter().zip(b.iter()) {
            if !const_eq(x, y)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    if p.is_type(crate::types::set::frozenset_type()) {
        let (a, b) = (crate::types::set::set_items(p), crate::types::set::set_items(q));
        if a.len() != b.len() {
            return Ok(false);
        }
        'outer: for x in &a {
            for y in &b {
                if const_eq(x, y)? {
                    continue 'outer;
                }
            }
            return Ok(false);
        }
        return Ok(true);
    }
    crate::abstract_::rich_eq(p, q)
}

unsafe extern "C" fn code_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        if (op != Py_EQ && op != Py_NE) || !is_code(&*b) {
            return none::not_implemented().to_ref().into_raw();
        }
        let x = &*(a as *mut PyCodeObject);
        let y = &*(b as *mut PyCodeObject);
        let r = (|| -> PyResult<bool> {
            if a == b {
                return Ok(true);
            }
            let (mx, my) = (&*x.meta, &*y.meta);
            if x.co_flags != y.co_flags
                || x.co_argcount != y.co_argcount
                || x.co_posonlyargcount != y.co_posonlyargcount
                || x.co_kwonlyargcount != y.co_kwonlyargcount
                || x.co_firstlineno != y.co_firstlineno
                || x.co_stacksize != y.co_stacksize
                || mx.public != my.public
                || bytes::bytes_data(&mx.co_code) != bytes::bytes_data(&my.co_code)
            {
                return Ok(false);
            }
            for (p, q) in [
                (x.co_name, y.co_name),
                (x.co_names, y.co_names),
                (x.co_localsplusnames, y.co_localsplusnames),
                (x.co_linetable, y.co_linetable),
                (x.co_exceptiontable, y.co_exceptiontable),
            ] {
                if !crate::abstract_::rich_eq(&*p, &*q)? {
                    return Ok(false);
                }
            }
            if mx.consts.len() != my.consts.len() {
                return Ok(false);
            }
            for (p, q) in mx.consts.iter().zip(my.consts.iter()) {
                if !const_eq(p, q)? {
                    return Ok(false);
                }
            }
            Ok(true)
        })();
        match r {
            Ok(eq) => crate::types::bool_::new_bool(if op == Py_EQ { eq } else { !eq }).into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}

unsafe extern "C" fn code_hash(op: *mut PyObject) -> Py_hash_t {
    unsafe {
        let c = &*(op as *mut PyCodeObject);
        let m = &*c.meta;
        let mut h = crate::hash::TupleHasher::new(8);
        h.add(pystr::str_hash(&*c.co_name));
        // bytecode: FNV-style over the words
        let mut bh: u64 = 0xcbf29ce484222325;
        for &b in bytes::bytes_data(&m.co_code) {
            bh = (bh ^ b as u64).wrapping_mul(0x100000001b3);
        }
        h.add((bh >> 1) as isize);
        h.add(c.co_firstlineno as isize);
        h.add(c.co_flags as isize);
        h.add(c.co_argcount as isize);
        h.add(c.co_posonlyargcount as isize);
        h.add(c.co_kwonlyargcount as isize);
        h.add(m.consts.len() as isize);
        h.finish()
    }
}

fn borrowed(p: *mut PyObject) -> PyResult {
    Ok(unsafe { PyRef::from_borrowed(p) })
}
fn co_varnames(o: &PyObject) -> PyResult {
    names_with_kind(o, bc::fast::LOCAL)
}
fn co_cellvars(o: &PyObject) -> PyResult {
    names_with_kind(o, bc::fast::CELL)
}
fn co_freevars(o: &PyObject) -> PyResult {
    names_with_kind(o, bc::fast::FREE)
}
fn names_with_kind(o: &PyObject, kind: u8) -> PyResult {
    let d = code_meta(o);
    Ok(tuple::new_tuple(names_of_kind(d, kind)))
}
fn names_of_kind(d: &CodeMeta, kind: u8) -> Vec<PyRef> {
    d.localsplusnames
        .iter()
        .zip(d.localspluskinds.iter())
        .filter(|(_, k)| **k & kind != 0 && !(kind == bc::fast::LOCAL && **k & bc::fast::HIDDEN != 0))
        .map(|(n, _)| n.clone())
        .collect()
}
fn co_nlocals(o: &PyObject) -> PyResult {
    Ok(int::new_int(names_of_kind(code_meta(o), bc::fast::LOCAL).len() as i64))
}
fn co_code(o: &PyObject) -> PyResult {
    Ok(code_meta(o).co_code.clone())
}
fn co_code_adaptive(o: &PyObject) -> PyResult {
    Ok(bytes::new_bytes(&crate::interp::adaptive::adaptive_code(o)))
}

/// (start, end, line) byte ranges, consecutive entries with the same line merged.
fn line_ranges(o: &PyObject) -> Vec<(u32, u32, Option<u32>)> {
    let c = get(o);
    let m = code_meta(o);
    let mut out: Vec<(u32, u32, Option<u32>)> = Vec::new();
    if !m.public {
        let d = code_data(o);
        for (i, l) in d.locations.iter().enumerate() {
            let line = if l.lineno == 0 { None } else { Some(l.lineno) };
            let (s, e) = (i as u32 * 4, i as u32 * 4 + 4);
            match out.last_mut() {
                Some(last) if last.2 == line && last.1 == s => last.1 = e,
                _ => out.push((s, e, line)),
            }
        }
        return out;
    }
    let lt = bytes::bytes_data(unsafe { &*c.co_linetable });
    let mut addr = 0u32;
    for e in wc::decode_linetable_entries(lt, c.co_firstlineno.max(0) as u32) {
        let line = e.pos.lineno;
        let (s, end) = (addr * 2, (addr + e.len) * 2);
        addr += e.len;
        match out.last_mut() {
            Some(last) if last.2 == line && last.1 == s => last.1 = end,
            _ => out.push((s, end, line)),
        }
    }
    out
}

fn co_lines(o: &PyObject) -> PyResult {
    let out: Vec<PyRef> = line_ranges(o)
        .into_iter()
        .map(|(s, e, l)| {
            tuple::new_tuple(vec![
                int::new_int(s as i64),
                int::new_int(e as i64),
                l.map_or_else(none::none_ref, |l| int::new_int(l as i64)),
            ])
        })
        .collect();
    let lst = crate::types::list::new_list(out);
    crate::abstract_::get_iter(&lst)
}

/// `-X no_debug_ranges` / PYTHONNODEBUGRANGES: code objects report no column ranges.
pub fn no_debug_ranges() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        let c = crate::modules::sys_full::config();
        c.xoptions.iter().any(|x| x == "no_debug_ranges" || x.starts_with("no_debug_ranges="))
            || (c.ignore_environment == 0 && std::env::var_os("PYTHONNODEBUGRANGES").is_some_and(|v| !v.is_empty()))
    })
}

fn co_positions(o: &PyObject) -> PyResult {
    let mut out: Vec<PyRef> = Vec::new();
    let no_ranges = no_debug_ranges();
    let opt = |x: Option<u32>| -> PyRef { x.map_or_else(none::none_ref, |v| int::new_int(v as i64)) };
    if !code_meta(o).public {
        let d = code_data(o);
        let o32 = |x: u32, none_if_zero: bool| -> PyRef {
            if (none_if_zero && x == 0) || x == u32::MAX { none::none_ref() } else { int::new_int(x as i64) }
        };
        for l in &d.locations {
            let item = tuple::new_tuple(vec![
                o32(l.lineno, true),
                o32(if no_ranges { l.lineno } else { l.end_lineno }, true),
                if l.lineno == 0 || no_ranges { none::none_ref() } else { o32(l.col, false) },
                if l.lineno == 0 || no_ranges { none::none_ref() } else { o32(l.end_col, false) },
            ]);
            out.push(item.clone());
            out.push(item);
        }
    } else {
        let c = get(o);
        let lt = bytes::bytes_data(unsafe { &*c.co_linetable });
        // no location table at all: no positions (dis then reports None everywhere)
        let ncu = if lt.is_empty() { 0 } else { ncodeunits(o) };
        let mut n = 0usize;
        for e in wc::decode_linetable_entries(lt, c.co_firstlineno.max(0) as u32) {
            let p = e.pos;
            let item = if no_ranges {
                tuple::new_tuple(vec![opt(p.lineno), opt(p.lineno), none::none_ref(), none::none_ref()])
            } else {
                tuple::new_tuple(vec![opt(p.lineno), opt(p.end_lineno), opt(p.col), opt(p.end_col)])
            };
            for _ in 0..e.len {
                if n < ncu {
                    out.push(item.clone());
                    n += 1;
                }
            }
        }
        // units past the end of the table: no line, but an end line just after the
        // first line (what CPython's location iterator reports there)
        while n < ncu {
            let el = if no_ranges { none::none_ref() } else { int::new_int(c.co_firstlineno as i64 + 1) };
            out.push(tuple::new_tuple(vec![none::none_ref(), el, none::none_ref(), none::none_ref()]));
            n += 1;
        }
    }
    let lst = crate::types::list::new_list(out);
    crate::abstract_::get_iter(&lst)
}

/// co_branches(): (instruction offset, not-taken destination, taken destination) for
/// every conditional branch, in bytes.
fn co_branches(o: &PyObject) -> PyResult {
    let mut out: Vec<PyRef> = Vec::new();
    if code_meta(o).public {
        let code = bytes::bytes_data(&code_meta(o).co_code);
        for ins in wc::instructions(code) {
            let Some(op) = ins.op else { continue };
            use wc::op::*;
            let is_branch = matches!(op, POP_JUMP_IF_FALSE | POP_JUMP_IF_TRUE | POP_JUMP_IF_NONE | POP_JUMP_IF_NOT_NONE | FOR_ITER);
            if !is_branch && op != END_ASYNC_FOR {
                continue;
            }
            let (src, left, right) = if op == END_ASYNC_FOR {
                // END_ASYNC_FOR's argument points back at the SEND of the loop header
                let send = ins.next() as i64 - ins.arg as i64;
                (send.max(0) as u32, send.max(0) as u32 + 2, ins.next())
            } else {
                let t = wc::jump_target(&ins).unwrap_or(0);
                // an exhausted FOR_ITER continues past END_FOR and POP_ITER; the fall-through
                // of a conditional jump starts after its NOT_TAKEN
                let right = if op == FOR_ITER { t + 2 } else { t };
                let mut left = ins.next();
                if code.get(2 * left as usize).copied().and_then(wc::deopt) == Some(NOT_TAKEN) {
                    left += 1;
                }
                (ins.offset, left, right)
            };
            out.push(tuple::new_tuple(vec![
                int::new_int(src as i64 * 2),
                int::new_int(left as i64 * 2),
                int::new_int(right as i64 * 2),
            ]));
        }
    }
    let lst = crate::types::list::new_list(out);
    crate::abstract_::get_iter(&lst)
}
fn co_name_get(o: &PyObject) -> PyResult {
    borrowed(get(o).co_name)
}
fn co_qualname_get(o: &PyObject) -> PyResult {
    borrowed(get(o).co_qualname)
}
fn co_filename_get(o: &PyObject) -> PyResult {
    borrowed(get(o).co_filename)
}
fn co_consts_get(o: &PyObject) -> PyResult {
    borrowed(get(o).co_consts)
}
fn co_names_get(o: &PyObject) -> PyResult {
    borrowed(get(o).co_names)
}
fn co_linetable_get(o: &PyObject) -> PyResult {
    borrowed(get(o).co_linetable)
}
fn co_exceptiontable_get(o: &PyObject) -> PyResult {
    borrowed(get(o).co_exceptiontable)
}
fn int_field(v: c_int) -> PyResult {
    Ok(int::new_int(v as i64))
}
fn co_argcount(o: &PyObject) -> PyResult {
    int_field(get(o).co_argcount)
}
fn co_posonlyargcount(o: &PyObject) -> PyResult {
    int_field(get(o).co_posonlyargcount)
}
fn co_kwonlyargcount(o: &PyObject) -> PyResult {
    int_field(get(o).co_kwonlyargcount)
}
fn co_stacksize(o: &PyObject) -> PyResult {
    int_field(get(o).co_stacksize)
}
fn co_flags(o: &PyObject) -> PyResult {
    int_field(get(o).co_flags)
}
fn co_firstlineno(o: &PyObject) -> PyResult {
    int_field(get(o).co_firstlineno)
}

fn str_items(t: &PyObject, what: &str) -> PyResult<Vec<PyRef>> {
    if !tuple::is_tuple(t) {
        raise!(TypeError, "{} must be a tuple of strings, not {}", what, tname(t));
    }
    let v = tuple::tuple_to_vec(t);
    for n in &v {
        if !pystr::is_str(n) {
            raise!(TypeError, "name tuples must contain only strings, not '{}'", tname(n));
        }
    }
    Ok(v)
}

/// localsplus (names, kinds) from the CodeType(...) view: varnames (arguments first),
/// cellvars (sharing a variable's slot when the names match), freevars.
fn build_localsplus(argcount: i32, kwonly: i32, flags: i32, varnames: &[PyRef], cellvars: &[PyRef], freevars: &[PyRef]) -> (Vec<PyRef>, Vec<u8>) {
    use bc::fast;
    let npos = argcount.max(0) as usize;
    let nkw = kwonly.max(0) as usize;
    let fl = flags as u32;
    let has_va = fl & bc::co::VARARGS != 0;
    let has_vk = fl & bc::co::VARKEYWORDS != 0;
    let mut names: Vec<PyRef> = Vec::new();
    let mut kinds: Vec<u8> = Vec::new();
    for (i, n) in varnames.iter().enumerate() {
        let mut k = fast::LOCAL;
        if i < npos {
            k |= fast::ARG_POS | fast::ARG_KW;
        } else if i < npos + nkw {
            k |= fast::ARG_KW;
        } else if has_va && i == npos + nkw {
            k |= fast::ARG_VAR | fast::ARG_POS;
        } else if has_vk && i == npos + nkw + has_va as usize {
            k |= fast::ARG_VAR | fast::ARG_KW;
        }
        names.push(n.clone());
        kinds.push(k);
    }
    for n in cellvars {
        match names.iter().position(|x| pystr::str_eq(x, n)) {
            Some(i) => kinds[i] |= fast::CELL,
            None => {
                names.push(n.clone());
                kinds.push(fast::CELL);
            }
        }
    }
    for n in freevars {
        names.push(n.clone());
        kinds.push(fast::FREE);
    }
    (names, kinds)
}

/// Fix the positional-only marks: kinds for the first `posonly` arguments lose ARG_KW.
fn mark_posonly(kinds: &mut [u8], posonly: i32) {
    for k in kinds.iter_mut().take(posonly.max(0) as usize) {
        if *k & bc::fast::ARG_POS != 0 {
            *k &= !bc::fast::ARG_KW;
        }
    }
}

fn as_c_int(v: &PyObject, what: &str) -> PyResult<i32> {
    if !int::is_int(v) {
        raise!(TypeError, "'{}' must be int, not {}", what, tname(v));
    }
    let x = crate::abstract_::as_i64(v)?;
    if x < i32::MIN as i64 || x > i32::MAX as i64 {
        raise!(OverflowError, "Python int too large to convert to C int");
    }
    Ok(x as i32)
}

fn code_replace(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = crate::args::split(args, kw);
    if !pos.is_empty() {
        raise!(TypeError, "replace() takes no positional arguments");
    }
    let c = get(o);
    let m = code_meta(o);
    let mut argcount = c.co_argcount;
    let mut posonly = c.co_posonlyargcount;
    let mut kwonly = c.co_kwonlyargcount;
    let mut nlocals: Option<i32> = None;
    let mut stacksize = c.co_stacksize;
    let mut flags = c.co_flags;
    let mut firstlineno = c.co_firstlineno;
    let mut code_b: PyRef = m.co_code.clone();
    let mut consts: Vec<PyRef> = m.consts.clone();
    let mut names: Vec<PyRef> = m.names.clone();
    let mut varnames: Option<Vec<PyRef>> = None;
    let mut freevars: Option<Vec<PyRef>> = None;
    let mut cellvars: Option<Vec<PyRef>> = None;
    let mut filename = m.filename.clone();
    let mut name = m.co_name.clone();
    let mut qualname = m.co_qualname.clone();
    let mut linetable = unsafe { PyRef::from_borrowed(c.co_linetable) };
    let mut exctable = unsafe { PyRef::from_borrowed(c.co_exceptiontable) };
    for (k, v) in kws {
        let ks = pystr::to_string_lossy(k);
        let want_str = |v: &PyObject| -> PyResult<PyRef> {
            if !pystr::is_str(v) {
                raise!(TypeError, "replace() argument '{}' must be str, not {}", ks, tname(v));
            }
            Ok(v.to_ref())
        };
        let want_bytes = |v: &PyObject| -> PyResult<PyRef> {
            if !bytes::is_bytes(v) {
                raise!(TypeError, "replace() argument '{}' must be bytes, not {}", ks, tname(v));
            }
            Ok(v.to_ref())
        };
        let want_tuple = |v: &PyObject| -> PyResult<Vec<PyRef>> {
            if !tuple::is_tuple(v) {
                raise!(TypeError, "replace() argument '{}' must be tuple, not {}", ks, tname(v));
            }
            Ok(tuple::tuple_to_vec(v))
        };
        match ks.as_str() {
            "co_argcount" => argcount = as_c_int(v, &ks)?,
            "co_posonlyargcount" => posonly = as_c_int(v, &ks)?,
            "co_kwonlyargcount" => kwonly = as_c_int(v, &ks)?,
            "co_nlocals" => nlocals = Some(as_c_int(v, &ks)?),
            "co_stacksize" => stacksize = as_c_int(v, &ks)?,
            "co_flags" => flags = as_c_int(v, &ks)?,
            "co_firstlineno" => firstlineno = as_c_int(v, &ks)?,
            "co_code" => code_b = want_bytes(v)?,
            "co_consts" => consts = want_tuple(v)?,
            "co_names" => names = str_items(&tuple::new_tuple(want_tuple(v)?), "co_names")?,
            "co_varnames" => varnames = Some(str_items(&tuple::new_tuple(want_tuple(v)?), "co_varnames")?),
            "co_freevars" => freevars = Some(str_items(&tuple::new_tuple(want_tuple(v)?), "co_freevars")?),
            "co_cellvars" => cellvars = Some(str_items(&tuple::new_tuple(want_tuple(v)?), "co_cellvars")?),
            "co_filename" => filename = want_str(v)?,
            "co_name" => name = want_str(v)?,
            "co_qualname" => qualname = want_str(v)?,
            "co_linetable" => linetable = want_bytes(v)?,
            "co_exceptiontable" => exctable = want_bytes(v)?,
            _ => raise!(TypeError, "replace() got an unexpected keyword argument '{}'", ks),
        }
    }
    let (lpn, mut kinds) = if varnames.is_none() && freevars.is_none() && cellvars.is_none() {
        (m.localsplusnames.clone(), m.localspluskinds.clone())
    } else {
        let vn = varnames.unwrap_or_else(|| names_of_kind(m, bc::fast::LOCAL));
        let cv = cellvars.unwrap_or_else(|| names_of_kind(m, bc::fast::CELL));
        let fv = freevars.unwrap_or_else(|| names_of_kind(m, bc::fast::FREE));
        let (n, mut k) = build_localsplus(argcount, kwonly, flags, &vn, &cv, &fv);
        mark_posonly(&mut k, posonly);
        (n, k)
    };
    if let Some(nl) = nlocals {
        let actual = kinds.iter().filter(|&&k| k & bc::fast::LOCAL != 0 && k & bc::fast::HIDDEN == 0).count();
        if nl as usize != actual || nl < 0 {
            raise!(ValueError, "co_nlocals != len(co_varnames)");
        }
    }
    if !m.public {
        if bytes::bytes_data(&code_b) != bytes::bytes_data(&m.co_code)
            || !linetable.is(unsafe { &*c.co_linetable })
            || !exctable.is(unsafe { &*c.co_exceptiontable })
            || lpn.len() != m.localsplusnames.len()
        {
            raise!(ValueError, "replace(): changing the bytecode of a legacy code object is not supported");
        }
        let d = code_data(o);
        kinds.truncate(lpn.len());
        return new_code_from_parts(CodeParts {
            argcount: argcount as u32,
            posonlyargcount: posonly as u32,
            kwonlyargcount: kwonly as u32,
            flags: flags as u32,
            stacksize: stacksize as u32,
            firstlineno: firstlineno as u32,
            code: d.code.clone(),
            locations: d.locations.clone(),
            exception_table: d.exception_table.clone(),
            kinds,
            filename,
            consts,
            names: names.into_iter().map(pystr::intern).collect(),
            localsplusnames: lpn,
            name,
            qualname,
        });
    }
    new_code_from_public(PublicParts {
        argcount,
        posonlyargcount: posonly,
        kwonlyargcount: kwonly,
        stacksize,
        flags,
        firstlineno,
        code: code_b,
        consts,
        names,
        localsplusnames: lpn,
        kinds,
        filename,
        name,
        qualname,
        linetable,
        exceptiontable: exctable,
    })
}

/// types.CodeType(argcount, posonlyargcount, kwonlyargcount, nlocals, stacksize, flags,
///   codestring, constants, names, varnames, filename, name, qualname, firstlineno,
///   linetable, exceptiontable, freevars=(), cellvars=(), /)
unsafe extern "C" fn code_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if !kw.is_null() && crate::types::dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "code() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() < 16 || a.len() > 18 {
                raise!(
                    TypeError,
                    "code expected {} arguments, got {}",
                    if a.len() < 16 { "at least 16" } else { "at most 18" },
                    a.len()
                );
            }
            let argcount = as_c_int(a[0], "argcount")?;
            let posonly = as_c_int(a[1], "posonlyargcount")?;
            let kwonly = as_c_int(a[2], "kwonlyargcount")?;
            let nlocals = as_c_int(a[3], "nlocals")?;
            let stacksize = as_c_int(a[4], "stacksize")?;
            let flags = as_c_int(a[5], "flags")?;
            if !bytes::is_bytes(a[6]) {
                raise!(TypeError, "code() argument 7 must be bytes, not {}", tname(a[6]));
            }
            if !tuple::is_tuple(a[7]) {
                raise!(TypeError, "code() argument 8 must be tuple, not {}", tname(a[7]));
            }
            let names = str_items(a[8], "names")?;
            let varnames = str_items(a[9], "varnames")?;
            for (i, w) in [(10usize, "filename"), (11, "name"), (12, "qualname")] {
                if !pystr::is_str(a[i]) {
                    raise!(TypeError, "code() argument {} must be str, not {}", i + 1, tname(a[i]));
                    #[allow(unreachable_code)]
                    let _ = w;
                }
            }
            let firstlineno = as_c_int(a[13], "firstlineno")?;
            for i in [14usize, 15] {
                if !bytes::is_bytes(a[i]) {
                    raise!(TypeError, "code() argument {} must be bytes, not {}", i + 1, tname(a[i]));
                }
            }
            let freevars = if a.len() > 16 { str_items(a[16], "freevars")? } else { Vec::new() };
            let cellvars = if a.len() > 17 { str_items(a[17], "cellvars")? } else { Vec::new() };
            if argcount < 0 {
                raise!(ValueError, "code: argcount must not be negative");
            }
            if posonly < 0 {
                raise!(ValueError, "code: posonlyargcount must not be negative");
            }
            if kwonly < 0 {
                raise!(ValueError, "code: kwonlyargcount must not be negative");
            }
            if nlocals < 0 {
                raise!(ValueError, "code: nlocals must not be negative");
            }
            if nlocals as usize != varnames.len() {
                raise!(ValueError, "code: co_nlocals != len(co_varnames)");
            }
            let (lpn, mut kinds) = build_localsplus(argcount, kwonly, flags, &varnames, &cellvars, &freevars);
            mark_posonly(&mut kinds, posonly);
            new_code_from_public(PublicParts {
                argcount,
                posonlyargcount: posonly,
                kwonlyargcount: kwonly,
                stacksize,
                flags,
                firstlineno,
                code: a[6].to_ref(),
                consts: tuple::tuple_to_vec(a[7]),
                names,
                localsplusnames: lpn,
                kinds,
                filename: a[10].to_ref(),
                name: a[11].to_ref(),
                qualname: a[12].to_ref(),
                linetable: a[14].to_ref(),
                exceptiontable: a[15].to_ref(),
            })
        })();
        into_c(r)
    }
}

/// Name of localsplus slot `oparg` (used by dis).
fn code_varname_from_oparg(o: &PyObject, i: &PyObject) -> PyResult {
    let names = unsafe { PyRef::from_borrowed(get(o).co_localsplusnames) };
    let k = crate::abstract_::as_i64(i)?;
    if k < 0 || k as usize >= tuple::tuple_len(&names) {
        raise!(IndexError, "tuple index out of range");
    }
    Ok(tuple::tuple_to_vec(&names)[k as usize].clone())
}

fn code_sizeof(o: &PyObject) -> PyResult {
    let m = code_meta(o);
    Ok(int::new_int((std::mem::size_of::<PyCodeObject>() + bytes::bytes_data(&m.co_code).len()) as i64))
}

pub fn init_types() {
    unsafe {
        let t = &mut *code_type();
        t.tp_name = c"code".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyCodeObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_doc = c"code(argcount, posonlyargcount, kwonlyargcount, nlocals, stacksize,\n     flags, codestring, constants, names, varnames, filename, name,\n     qualname, firstlineno, linetable, exceptiontable, freevars=(),\n     cellvars=(), /)\n--\n\nCreate a code object.  Not for the faint of heart.".as_ptr();
        t.tp_dealloc = Some(code_dealloc);
        t.tp_repr = Some(code_repr);
        t.tp_richcompare = Some(code_richcompare);
        t.tp_hash = Some(code_hash);
        t.tp_new = Some(code_new);
        t.tp_weaklistoffset = std::mem::offset_of!(PyCodeObject, co_weakreflist) as isize;
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_getset = crate::getset_table![
            ("co_varnames", co_varnames),
            ("co_cellvars", co_cellvars),
            ("co_freevars", co_freevars),
            ("co_nlocals", co_nlocals),
            ("co_code", co_code),
            ("_co_code_adaptive", co_code_adaptive),
            ("co_name", co_name_get),
            ("co_qualname", co_qualname_get),
            ("co_filename", co_filename_get),
            ("co_consts", co_consts_get),
            ("co_names", co_names_get),
            ("co_linetable", co_linetable_get),
            ("co_exceptiontable", co_exceptiontable_get),
            ("co_argcount", co_argcount),
            ("co_posonlyargcount", co_posonlyargcount),
            ("co_kwonlyargcount", co_kwonlyargcount),
            ("co_stacksize", co_stacksize),
            ("co_flags", co_flags),
            ("co_firstlineno", co_firstlineno),
        ];
        t.tp_methods = crate::mdefs![
            ("co_lines", NOARGS, co_lines),
            ("co_positions", NOARGS, co_positions),
            ("co_branches", NOARGS, co_branches),
            ("replace", FASTKW, code_replace, "replace($self, /, *, co_argcount=-1, co_posonlyargcount=-1, co_kwonlyargcount=-1,\n        co_nlocals=-1, co_stacksize=-1, co_flags=-1, co_firstlineno=-1,\n        co_code=None, co_consts=None, co_names=None, co_varnames=None,\n        co_freevars=None, co_cellvars=None, co_filename=None, co_name=None,\n        co_qualname=None, co_linetable=None, co_exceptiontable=None)\n--\n\nReturn a copy of the code object with the given fields replaced."),
            ("__replace__", FASTKW, code_replace, "__replace__($self, /, **changes)\n--\n\nReturn a copy of the code object with the given fields replaced."),
            ("__sizeof__", NOARGS, code_sizeof),
            ("_varname_from_oparg", O, code_varname_from_oparg, "_varname_from_oparg($self, /, oparg)\n--\n\n(internal-only) Return the local variable name for the given oparg."),
        ];
    }
}
