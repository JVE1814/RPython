//! Code objects. The struct starts with CPython's public `PyCodeObject` prefix
//! (fields extensions may read); the executable bytecode and side tables live in
//! a Rust-owned `CodeData`.

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{bytes, int, none, str as pystr, tuple};
use crate::raise;
use rpy_compiler::bytecode::{self as bc, CodeUnit, Constant, ExceptionEntry, Location};
use rpy_compiler::wordcode as wc;
use std::ffi::{c_int, c_void};

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
    /// Rust side data (owned Box).
    pub data: *mut CodeData,
}

/// Executable representation.
pub struct CodeData {
    pub code: Vec<u32>,
    pub locations: Vec<Location>,
    pub exception_table: Vec<ExceptionEntry>,
    /// Constants as objects (same as co_consts, kept as a Vec for fast indexing).
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
    /// Transitional (D24, see notes/bytecode.md): the public CPython 3.15 form produced by
    /// the wordcode pipeline, shown by co_code / co_consts / co_names / co_linetable /
    /// co_exceptiontable / co_lines / co_positions while execution still uses `code`.
    pub public: std::sync::OnceLock<Box<PublicView>>,
}

/// Public (CPython 3.15) view of a code object; see [`CodeData::public`].
pub struct PublicView {
    pub code: PyRef,
    pub consts: PyRef,
    pub names: PyRef,
    pub linetable: PyRef,
    pub exceptiontable: PyRef,
    pub stacksize: u32,
}

/// Convenience accessor bundle.
pub struct CodeRef<'a> {
    pub obj: &'a PyCodeObject,
    pub d: &'a CodeData,
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

pub fn code(o: &PyObject) -> CodeRef<'_> {
    unsafe {
        let c = &*(o.as_ptr() as *const PyCodeObject);
        let d = &*c.data;
        CodeRef { obj: c, d, co_name: &d.co_name, co_qualname: &d.co_qualname }
    }
}

#[inline(always)]
pub fn code_data(o: &PyObject) -> &CodeData {
    unsafe { &*(*(o.as_ptr() as *const PyCodeObject)).data }
}

/// Convert a compiler constant into an object.
pub fn const_to_obj(c: &Constant, filename: &PyRef) -> PyResult {
    Ok(match c {
        Constant::None => none::none_ref(),
        Constant::Bool(b) => crate::types::bool_::new_bool(*b),
        Constant::Ellipsis => none::ellipsis().to_ref(),
        Constant::Int(i) => int::new_int(*i),
        Constant::BigInt { neg, decimal } => crate::types::int_methods::from_decimal_str(*neg, decimal)?,
        Constant::Float(f) => crate::types::float::new_float(*f),
        Constant::Complex { real, imag } => crate::types::complex::new_complex(*real, *imag),
        Constant::Str(cps) => pystr::intern(pystr::from_codepoints(cps)),
        Constant::Bytes(b) => bytes::new_bytes(b),
        Constant::Tuple(items) => {
            let mut v = Vec::with_capacity(items.len());
            for it in items {
                v.push(const_to_obj(it, filename)?);
            }
            tuple::new_tuple(v)
        }
        Constant::FrozenSet(items) => {
            let mut v = Vec::with_capacity(items.len());
            for it in items {
                v.push(const_to_obj(it, filename)?);
            }
            crate::types::set::new_frozenset_from_vec(v)?
        }
        Constant::Code(cu) => new_code_from_unit(cu, Some(filename))?,
    })
}

/// Build a code object from a compiled unit.
pub fn new_code_from_unit(cu: &CodeUnit, filename: Option<&PyRef>) -> PyResult {
    new_code_from_unit_public(cu, None, filename)
}

/// Like [`new_code_from_unit`], attaching the public (wordcode) form `public` of the same
/// unit as the code object's public view (transitional, D24). Nested code objects are
/// paired by their order in the constant tables.
pub fn new_code_from_unit_public(cu: &CodeUnit, public: Option<&wc::CodeObject>, filename: Option<&PyRef>) -> PyResult {
    let filename = match filename {
        Some(f) => f.clone(),
        None => pystr::intern_str(&cu.filename),
    };
    let pub_codes: Vec<&wc::CodeObject> = public
        .map(|p| p.consts.iter().filter_map(|c| if let wc::Constant::Code(x) = c { Some(&**x) } else { None }).collect())
        .unwrap_or_default();
    let n_codes = cu.consts.iter().filter(|c| matches!(c, Constant::Code(_))).count();
    let paired = public.is_some() && pub_codes.len() == n_codes;
    let mut consts = Vec::with_capacity(cu.consts.len());
    let mut code_objs: Vec<PyRef> = Vec::new();
    for c in &cu.consts {
        match c {
            Constant::Code(inner) => {
                let p = if paired { Some(pub_codes[code_objs.len()]) } else { None };
                let o = new_code_from_unit_public(inner, p, Some(&filename))?;
                code_objs.push(o.clone());
                consts.push(o);
            }
            _ => consts.push(const_to_obj(c, &filename)?),
        }
    }
    let names: Vec<PyRef> = cu.names.iter().map(|n| pystr::intern_str(n)).collect();
    let lpn: Vec<PyRef> = cu.localsplusnames.iter().map(|n| pystr::intern_str(n)).collect();
    let parts = CodeParts {
        argcount: cu.argcount,
        posonlyargcount: cu.posonlyargcount,
        kwonlyargcount: cu.kwonlyargcount,
        flags: cu.flags,
        stacksize: cu.stacksize,
        firstlineno: cu.firstlineno,
        code: cu.code.clone(),
        locations: cu.locations.clone(),
        exception_table: cu.exception_table.clone(),
        kinds: cu.localspluskinds.clone(),
        filename,
        consts,
        names,
        localsplusnames: lpn,
        name: pystr::intern_str(&cu.name),
        qualname: pystr::intern_str(&cu.qualname),
    };
    let fname = parts.filename.clone();
    let r = new_code_from_parts(parts)?;
    if let Some(p) = public {
        let mut nested = code_objs.into_iter();
        if let Ok(view) = public_view(p, if paired { Some(&mut nested) } else { None }, &fname) {
            let _ = code_data(&r).public.set(Box::new(view));
        } else {
            clear_exc();
        }
    }
    Ok(r)
}

/// Constant of the public form as an object (nested code objects come from `nested`).
fn public_const_to_obj(c: &wc::Constant, nested: &mut Option<&mut std::vec::IntoIter<PyRef>>, filename: &PyRef) -> PyResult {
    Ok(match c {
        wc::Constant::None => none::none_ref(),
        wc::Constant::Bool(b) => crate::types::bool_::new_bool(*b),
        wc::Constant::Ellipsis => none::ellipsis().to_ref(),
        wc::Constant::Int(i) => int::new_int(*i),
        wc::Constant::BigInt { neg, decimal } => crate::types::int_methods::from_decimal_str(*neg, decimal)?,
        wc::Constant::Float(f) => crate::types::float::new_float(*f),
        wc::Constant::Complex { real, imag } => crate::types::complex::new_complex(*real, *imag),
        wc::Constant::Str(cps) => pystr::intern(pystr::from_codepoints(cps)),
        wc::Constant::Bytes(b) => bytes::new_bytes(b),
        wc::Constant::Tuple(items) => {
            let mut v = Vec::with_capacity(items.len());
            for it in items {
                v.push(public_const_to_obj(it, nested, filename)?);
            }
            tuple::new_tuple(v)
        }
        wc::Constant::FrozenSet(items) => {
            let mut v = Vec::with_capacity(items.len());
            for it in items {
                v.push(public_const_to_obj(it, nested, filename)?);
            }
            crate::types::set::new_frozenset_from_vec(v)?
        }
        wc::Constant::Slice(s) => {
            let a = public_const_to_obj(&s[0], nested, filename)?;
            let b = public_const_to_obj(&s[1], nested, filename)?;
            let c = public_const_to_obj(&s[2], nested, filename)?;
            crate::types::slice::new_slice(&a, &b, &c)
        }
        wc::Constant::Code(_) => match nested.as_mut().and_then(|it| it.next()) {
            Some(o) => o,
            None => raise!(SystemError, "unpaired code constant"),
        },
    })
}

fn public_view(p: &wc::CodeObject, mut nested: Option<&mut std::vec::IntoIter<PyRef>>, filename: &PyRef) -> PyResult<PublicView> {
    let mut consts = Vec::with_capacity(p.consts.len());
    for c in &p.consts {
        consts.push(public_const_to_obj(c, &mut nested, filename)?);
    }
    Ok(PublicView {
        code: bytes::new_bytes(&p.code),
        consts: tuple::new_tuple(consts),
        names: tuple::new_tuple(p.names.iter().map(|n| pystr::intern_str(n)).collect()),
        linetable: bytes::new_bytes(&p.linetable),
        exceptiontable: bytes::new_bytes(&p.exceptiontable),
        stacksize: p.stacksize,
    })
}

/// Everything a code object is made of, with the object-valued parts already built
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

pub fn new_code_from_parts(p: CodeParts) -> PyResult {
    let kinds = p.kinds;
    let nlocals = kinds.iter().filter(|&&k| k & bc::fast::LOCAL != 0).count();
    let ncell = kinds.iter().filter(|&&k| k & bc::fast::CELL != 0).count();
    let nfree = kinds.iter().filter(|&&k| k & bc::fast::FREE != 0).count();
    let cell_slots: Vec<u32> =
        kinds.iter().enumerate().filter(|(_, k)| **k & bc::fast::CELL != 0).map(|(i, _)| i as u32).collect();
    let exctable = encode_exception_table(&p.exception_table);
    let linetable = encode_locations(&p.locations);
    let ninstr = p.code.len();
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
        public: std::sync::OnceLock::new(),
    });
    unsafe {
        let op = crate::alloc::alloc_object(code_type(), std::mem::size_of::<PyCodeObject>());
        let c = &mut *(op as *mut PyCodeObject);
        c.co_consts = tuple::new_tuple(p.consts).into_raw();
        c.co_names = tuple::new_tuple(p.names).into_raw();
        c.co_exceptiontable = bytes::new_bytes(&exctable).into_raw();
        c.co_flags = p.flags as c_int;
        c.co_argcount = p.argcount as c_int;
        c.co_posonlyargcount = p.posonlyargcount as c_int;
        c.co_kwonlyargcount = p.kwonlyargcount as c_int;
        c.co_stacksize = p.stacksize as c_int;
        c.co_firstlineno = p.firstlineno as c_int;
        c.co_nlocalsplus = kinds.len() as c_int;
        c.co_framesize = (kinds.len() + p.stacksize as usize) as c_int;
        c.co_nlocals = nlocals as c_int;
        c.co_ncellvars = ncell as c_int;
        c.co_nfreevars = nfree as c_int;
        c.co_version = 1;
        c.co_localsplusnames = tuple::new_tuple(p.localsplusnames).into_raw();
        c.co_localspluskinds = bytes::new_bytes(&kinds).into_raw();
        c.co_filename = p.filename.into_raw();
        c.co_name = p.name.into_raw();
        c.co_qualname = p.qualname.into_raw();
        c.co_linetable = bytes::new_bytes(&linetable).into_raw();
        c.data = Box::into_raw(data);
        Ok(PyRef::from_raw(op))
    }
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

/// Our own linetable encoding: per instruction (lineno, end_lineno, col, end_col) as u32 LE.
fn encode_locations(locs: &[Location]) -> Vec<u8> {
    let mut v = Vec::with_capacity(locs.len() * 16);
    for l in locs {
        for x in [l.lineno, l.end_lineno, l.col, l.end_col] {
            v.extend_from_slice(&x.to_le_bytes());
        }
    }
    v
}

/// Line number for an instruction index (walking back over artificial ones).
pub fn line_for_instr(co: &PyObject, i: usize) -> Option<u32> {
    let d = code_data(co);
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

unsafe extern "C" fn code_dealloc(op: *mut PyObject) {
    unsafe {
        let c = &mut *(op as *mut PyCodeObject);
        crate::types::weakref::clear_weakrefs_at(op, &mut c.co_weakreflist);
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
        if !c.data.is_null() {
            drop(Box::from_raw(c.data));
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
            let dx = &*x.data;
            let dy = &*y.data;
            if dx.code != dy.code
                || x.co_flags != y.co_flags
                || x.co_argcount != y.co_argcount
                || x.co_posonlyargcount != y.co_posonlyargcount
                || x.co_kwonlyargcount != y.co_kwonlyargcount
                || x.co_firstlineno != y.co_firstlineno
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
            if dx.consts.len() != dy.consts.len() {
                return Ok(false);
            }
            for (p, q) in dx.consts.iter().zip(dy.consts.iter()) {
                // constants must have the same type too (1 vs 1.0 vs True)
                if p.type_ptr() != q.type_ptr() || !crate::abstract_::rich_eq(p, q)? {
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
        let d = &*c.data;
        let mut h = crate::hash::TupleHasher::new(4);
        h.add(pystr::str_hash(&*c.co_name));
        h.add(d.code.len() as isize);
        h.add(c.co_firstlineno as isize);
        h.add(c.co_flags as isize);
        h.finish()
    }
}

fn get(o: &PyObject) -> &PyCodeObject {
    unsafe { &*(o.as_ptr() as *const PyCodeObject) }
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
    let d = code_data(o);
    let v: Vec<PyRef> = d
        .localsplusnames
        .iter()
        .zip(d.localspluskinds.iter())
        .filter(|(_, k)| **k & kind != 0 && !(kind == bc::fast::LOCAL && **k & bc::fast::HIDDEN != 0))
        .map(|(n, _)| n.clone())
        .collect();
    Ok(tuple::new_tuple(v))
}
fn co_nlocals(o: &PyObject) -> PyResult {
    Ok(int::new_int(tuple::tuple_len(&*co_varnames(o)?) as i64))
}
fn co_code(o: &PyObject) -> PyResult {
    if let Some(p) = code_data(o).public.get() {
        return Ok(p.code.clone());
    }
    let d = code_data(o);
    let mut v = Vec::with_capacity(d.code.len() * 4);
    for w in &d.code {
        v.extend_from_slice(&w.to_le_bytes());
    }
    Ok(bytes::new_bytes(&v))
}
fn co_lines(o: &PyObject) -> PyResult {
    if let Some(p) = code_data(o).public.get() {
        return public_co_lines(o, p);
    }
    // (start_offset, end_offset, lineno) with offsets in bytes (4 per instruction)
    let d = code_data(o);
    let mut out: Vec<PyRef> = Vec::new();
    let mut i = 0;
    while i < d.locations.len() {
        let line = d.locations[i].lineno;
        let mut j = i + 1;
        while j < d.locations.len() && d.locations[j].lineno == line {
            j += 1;
        }
        let l = if line == 0 { none::none_ref() } else { int::new_int(line as i64) };
        out.push(tuple::new_tuple(vec![int::new_int((i * 4) as i64), int::new_int((j * 4) as i64), l]));
        i = j;
    }
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
    if let Some(p) = code_data(o).public.get() {
        return public_co_positions(o, p);
    }
    let d = code_data(o);
    let mut out: Vec<PyRef> = Vec::new();
    let opt = |x: u32, none_if_zero: bool| -> PyRef {
        if (none_if_zero && x == 0) || x == u32::MAX { none::none_ref() } else { int::new_int(x as i64) }
    };
    let no_ranges = no_debug_ranges();
    for l in &d.locations {
        let item = tuple::new_tuple(vec![
            opt(l.lineno, true),
            opt(l.end_lineno, true),
            if l.lineno == 0 || no_ranges { none::none_ref() } else { opt(l.col, false) },
            if l.lineno == 0 || no_ranges { none::none_ref() } else { opt(l.end_col, false) },
        ]);
        // CPython yields one entry per code unit; ours are 4-byte instructions = 2 code units
        out.push(item.clone());
        out.push(item);
    }
    let lst = crate::types::list::new_list(out);
    crate::abstract_::get_iter(&lst)
}
/// Per-code-unit positions decoded from the public location table.
fn public_positions(o: &PyObject, p: &PublicView) -> Vec<wc::Position> {
    let code = bytes::bytes_data(&p.code);
    let lt = bytes::bytes_data(&p.linetable);
    wc::decode_linetable(lt, get(o).co_firstlineno.max(0) as u32, code.len() / 2)
}

fn public_co_lines(o: &PyObject, p: &PublicView) -> PyResult {
    // (start, end, line) in bytes, merging consecutive code units on the same line
    let pos = public_positions(o, p);
    let mut out: Vec<PyRef> = Vec::new();
    let mut i = 0;
    while i < pos.len() {
        let line = pos[i].lineno;
        let mut j = i + 1;
        while j < pos.len() && pos[j].lineno == line {
            j += 1;
        }
        let l = match line {
            Some(l) => int::new_int(l as i64),
            None => none::none_ref(),
        };
        out.push(tuple::new_tuple(vec![int::new_int((i * 2) as i64), int::new_int((j * 2) as i64), l]));
        i = j;
    }
    crate::abstract_::get_iter(&crate::types::list::new_list(out))
}

fn public_co_positions(o: &PyObject, p: &PublicView) -> PyResult {
    let pos = public_positions(o, p);
    let no_ranges = no_debug_ranges();
    let opt = |x: Option<u32>| match x {
        Some(v) => int::new_int(v as i64),
        None => none::none_ref(),
    };
    let out: Vec<PyRef> = pos
        .iter()
        .map(|q| {
            tuple::new_tuple(vec![
                opt(q.lineno),
                opt(q.end_lineno),
                if no_ranges { none::none_ref() } else { opt(q.col) },
                if no_ranges { none::none_ref() } else { opt(q.end_col) },
            ])
        })
        .collect();
    crate::abstract_::get_iter(&crate::types::list::new_list(out))
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
    if let Some(p) = code_data(o).public.get() {
        return Ok(p.consts.clone());
    }
    borrowed(get(o).co_consts)
}
fn co_names_get(o: &PyObject) -> PyResult {
    if let Some(p) = code_data(o).public.get() {
        return Ok(p.names.clone());
    }
    borrowed(get(o).co_names)
}
fn co_linetable_get(o: &PyObject) -> PyResult {
    if let Some(p) = code_data(o).public.get() {
        return Ok(p.linetable.clone());
    }
    borrowed(get(o).co_linetable)
}
fn co_exceptiontable_get(o: &PyObject) -> PyResult {
    if let Some(p) = code_data(o).public.get() {
        return Ok(p.exceptiontable.clone());
    }
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
    if let Some(p) = code_data(o).public.get() {
        return Ok(int::new_int(p.stacksize as i64));
    }
    int_field(get(o).co_stacksize)
}
fn co_flags(o: &PyObject) -> PyResult {
    int_field(get(o).co_flags)
}
fn co_firstlineno(o: &PyObject) -> PyResult {
    int_field(get(o).co_firstlineno)
}
fn code_replace(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = crate::args::split(args, kw);
    if !pos.is_empty() {
        raise!(TypeError, "replace() takes no positional arguments");
    }
    let d = code_data(o);
    let c = get(o);
    let mut cu = CodeUnit {
        name: pystr::to_string_lossy(&d.co_name),
        qualname: pystr::to_string_lossy(&d.co_qualname),
        filename: pystr::to_string_lossy(&d.filename),
        firstlineno: c.co_firstlineno as u32,
        argcount: c.co_argcount as u32,
        posonlyargcount: c.co_posonlyargcount as u32,
        kwonlyargcount: c.co_kwonlyargcount as u32,
        flags: c.co_flags as u32,
        stacksize: c.co_stacksize as u32,
        code: d.code.clone(),
        locations: d.locations.clone(),
        consts: Vec::new(),
        names: d.names.iter().map(|n| pystr::to_string_lossy(n)).collect(),
        localsplusnames: d.localsplusnames.iter().map(|n| pystr::to_string_lossy(n)).collect(),
        localspluskinds: d.localspluskinds.clone(),
        exception_table: d.exception_table.clone(),
    };
    let mut new_consts: Option<PyRef> = None;
    let mut new_names: Option<PyRef> = None;
    let mut new_linetable: Option<PyRef> = None;
    let mut new_exctable: Option<PyRef> = None;
    for (k, v) in kws {
        let ks = pystr::to_string_lossy(k);
        let as_int = || -> PyResult<u32> { Ok(crate::abstract_::as_i64(v)? as u32) };
        let as_str = || -> PyResult<String> {
            if !pystr::is_str(v) {
                raise!(TypeError, "replace() argument '{}' must be str, not {}", ks, tname(v));
            }
            Ok(pystr::to_string_lossy(v))
        };
        match ks.as_str() {
            "co_name" => cu.name = as_str()?,
            "co_qualname" => cu.qualname = as_str()?,
            "co_filename" => cu.filename = as_str()?,
            "co_firstlineno" => cu.firstlineno = as_int()?,
            "co_argcount" => cu.argcount = as_int()?,
            "co_posonlyargcount" => cu.posonlyargcount = as_int()?,
            "co_kwonlyargcount" => cu.kwonlyargcount = as_int()?,
            "co_flags" => cu.flags = as_int()?,
            "co_stacksize" => cu.stacksize = as_int()?,
            "co_consts" => new_consts = Some(v.to_ref()),
            "co_names" => new_names = Some(v.to_ref()),
            "co_linetable" | "co_exceptiontable" if d.public.get().is_some() => {
                if !bytes::is_bytes(v) {
                    raise!(TypeError, "replace() argument '{}' must be bytes, not {}", ks, tname(v));
                }
                if ks == "co_linetable" {
                    new_linetable = Some(v.to_ref());
                } else {
                    new_exctable = Some(v.to_ref());
                }
            }
            "co_code" if d.public.get().is_some_and(|p| bytes::is_bytes(v) && bytes::bytes_data(v) == bytes::bytes_data(&p.code)) => {}
            "co_varnames" | "co_freevars" | "co_cellvars" | "co_nlocals" | "co_code" | "co_linetable"
            | "co_exceptiontable" => {
                raise!(ValueError, "replace(): changing {} is not supported by this implementation", ks)
            }
            _ => raise!(TypeError, "replace() got an unexpected keyword argument '{}'", ks),
        }
    }
    let keep_public = new_consts.is_none() && new_names.is_none();
    let r = new_code_from_unit(&cu, None)?;
    if let (true, Some(p)) = (keep_public, d.public.get()) {
        let view = PublicView {
            code: p.code.clone(),
            consts: p.consts.clone(),
            names: p.names.clone(),
            linetable: new_linetable.unwrap_or_else(|| p.linetable.clone()),
            exceptiontable: new_exctable.unwrap_or_else(|| p.exceptiontable.clone()),
            stacksize: p.stacksize,
        };
        let _ = code_data(&r).public.set(Box::new(view));
    }
    unsafe {
        let rc = &mut *(r.as_ptr() as *mut PyCodeObject);
        let rd = &mut *rc.data;
        let consts = match new_consts {
            Some(c) => crate::abstract_::sequence_tuple(&c)?,
            None => PyRef::from_borrowed(c.co_consts),
        };
        rd.consts = tuple::tuple_to_vec(&consts);
        let old = rc.co_consts;
        rc.co_consts = consts.into_raw();
        xdecref(old);
        if let Some(n) = new_names {
            let t = crate::abstract_::sequence_tuple(&n)?;
            rd.names = tuple::tuple_to_vec(&t);
            let old = rc.co_names;
            rc.co_names = t.into_raw();
            xdecref(old);
        }
    }
    Ok(r)
}
fn code_sizeof(o: &PyObject) -> PyResult {
    let d = code_data(o);
    Ok(int::new_int((std::mem::size_of::<PyCodeObject>() + d.code.len() * 4) as i64))
}

pub fn init_types() {
    unsafe {
        let t = &mut *code_type();
        t.tp_name = c"code".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyCodeObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_dealloc = Some(code_dealloc);
        t.tp_repr = Some(code_repr);
        t.tp_richcompare = Some(code_richcompare);
        t.tp_hash = Some(code_hash);
        t.tp_weaklistoffset = std::mem::offset_of!(PyCodeObject, co_weakreflist) as isize;
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_getset = crate::getset_table![
            ("co_varnames", co_varnames),
            ("co_cellvars", co_cellvars),
            ("co_freevars", co_freevars),
            ("co_nlocals", co_nlocals),
            ("co_code", co_code),
            ("_co_code_adaptive", co_code),
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
            ("replace", FASTKW, code_replace, "replace($self, /, *, co_argcount=-1, co_posonlyargcount=-1, co_kwonlyargcount=-1,\n        co_nlocals=-1, co_stacksize=-1, co_flags=-1, co_firstlineno=-1,\n        co_code=None, co_consts=None, co_names=None, co_varnames=None,\n        co_freevars=None, co_cellvars=None, co_filename=None, co_name=None,\n        co_qualname=None, co_linetable=None, co_exceptiontable=None)\n--\n\nReturn a copy of the code object with the given fields replaced."),
            ("__replace__", FASTKW, code_replace, "__replace__($self, /, **changes)\n--\n\nReturn a copy of the code object with the given fields replaced."),
            ("__sizeof__", NOARGS, code_sizeof),
        ];
    }
    let _ = std::ptr::null::<c_void>();
}
