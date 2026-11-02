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
    /// Rust side data (owned Box); null while the body is not loaded yet.
    pub data: *mut CodeData,
    /// Where to load the body from (perf2: lazily unmarshalled code; owned Box).
    pub lazy: *mut LazyBody,
    /// Docstring of a code object whose body is not loaded (owned; else null).
    pub doc: *mut PyObject,
}

/// A marshalled code body not unmarshalled yet: most functions of an imported module
/// never run, so their bytecode, constants and names are only built on first use
/// (`code_data`). `src` is the bytes object the module was loaded from.
pub struct LazyBody {
    pub src: PyRef,
    pub off: usize,
    pub len: usize,
    pub allow_code: bool,
}

/// Executable representation.
pub struct CodeData {
    pub code: Vec<u32>,
    /// Source positions (compact encoding shared with `co_linetable`).
    pub locations: LineTable,
    pub exception_table: Vec<ExceptionEntry>,
    /// Constants: a view of the code's own constants tuple (also `co_consts`).
    pub consts: RefTuple,
    /// Names (interned str objects).
    pub names: RefTuple,
    pub localsplusnames: RefTuple,
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
}

/// An owned exact tuple seen as a slice of owned references (perf2: code objects keep
/// one copy of their constants and names instead of a tuple plus a vector).
pub struct RefTuple(PyRef);

impl RefTuple {
    /// `t` must be an exact tuple.
    pub fn new(t: PyRef) -> RefTuple {
        debug_assert!(tuple::is_tuple_exact(&t));
        RefTuple(t)
    }
    pub fn from_vec(v: Vec<PyRef>) -> RefTuple {
        RefTuple(tuple::new_tuple(v))
    }
    pub fn tuple(&self) -> &PyRef {
        &self.0
    }
}

impl std::ops::Deref for RefTuple {
    type Target = [PyRef];
    #[inline(always)]
    fn deref(&self) -> &[PyRef] {
        let items = tuple::tuple_items(&self.0);
        // tuple items are non-null object pointers; PyRef is a transparent non-null pointer
        unsafe { std::slice::from_raw_parts(items.as_ptr() as *const PyRef, items.len()) }
    }
}

// ---------------------------------------------------------------- line table
//
// Source positions are stored once, in the `co_linetable` bytes object, in the
// location-table format that test_code.py's `parse_location_table` reads (perf2; it
// replaced 2 x 16 bytes per instruction). Entries describe runs of code units (one of
// our 4-byte instructions counts as two 2-byte units, like `co_positions` reports):
//
//   first byte: 0x80 | code << 3 | (units - 1), units <= 8
//   code 15      no location
//   code 14      svarint line delta, varint end-line delta, varint col+1, varint end_col+1
//   code 13      svarint line delta, no columns
//   code 10..12  line delta = code - 10, then column and end column bytes
//   code 0..9    same line; column = code << 3 | (next byte >> 4),
//                end column = column + (next byte & 15)
//
// Varints are little-endian groups of 6 bits, bit 6 = continuation; signed ones keep
// the sign in bit 0. Lines are relative to the previous entry, starting at
// co_firstlineno. Random access (`get`) uses checkpoints built on first use.

const LT_CHUNK: usize = 32;

#[inline]
fn put_varint(v: &mut Vec<u8>, mut x: u64) {
    while x >= 64 {
        v.push((x & 63) as u8 | 64);
        x >>= 6;
    }
    v.push(x as u8);
}
#[inline]
fn put_svarint(v: &mut Vec<u8>, x: i64) {
    put_varint(v, if x < 0 { ((x.unsigned_abs()) << 1) | 1 } else { (x as u64) << 1 });
}
#[inline]
fn get_varint(b: &[u8], pos: &mut usize) -> Option<u64> {
    let mut c = *b.get(*pos)?;
    *pos += 1;
    let mut x = (c & 63) as u64;
    let mut shift = 0;
    while c & 64 != 0 {
        c = *b.get(*pos)?;
        *pos += 1;
        shift += 6;
        if shift > 60 {
            return None;
        }
        x |= ((c & 63) as u64) << shift;
    }
    Some(x)
}
#[inline]
fn get_svarint(b: &[u8], pos: &mut usize) -> Option<i64> {
    let u = get_varint(b, pos)?;
    Some(if u & 1 != 0 { -((u >> 1) as i64) } else { (u >> 1) as i64 })
}

/// Encode positions (one per instruction) in the location-table format.
pub fn encode_lines(locs: &[Location], firstlineno: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(locs.len() * 2 + 4);
    let mut line = firstlineno as i64;
    let mut i = 0;
    while i < locs.len() {
        let l = locs[i];
        let mut n = 1;
        while n < 4 && i + n < locs.len() && locs[i + n] == l {
            n += 1;
        }
        i += n;
        let units = (2 * n - 1) as u8;
        if l.lineno == 0 {
            v.push(0x80 | 15 << 3 | units);
            continue;
        }
        let delta = l.lineno as i64 - line;
        let same_line = l.end_lineno == l.lineno;
        let cols = l.col != u32::MAX && l.end_col != u32::MAX;
        if same_line && !cols && l.col == u32::MAX && l.end_col == u32::MAX {
            v.push(0x80 | 13 << 3 | units);
            put_svarint(&mut v, delta);
        } else if same_line && cols && delta == 0 && l.col < 80 && l.end_col >= l.col && l.end_col - l.col < 16 {
            v.push(0x80 | ((l.col >> 3) as u8) << 3 | units);
            v.push(((l.col & 7) as u8) << 4 | (l.end_col - l.col) as u8);
        } else if same_line && cols && (0..3).contains(&delta) && l.col < 128 && l.end_col < 128 {
            v.push(0x80 | (10 + delta as u8) << 3 | units);
            v.push(l.col as u8);
            v.push(l.end_col as u8);
        } else {
            v.push(0x80 | 14 << 3 | units);
            put_svarint(&mut v, delta);
            put_varint(&mut v, (l.end_lineno as i64 - l.lineno as i64).max(0) as u64);
            put_varint(&mut v, if l.col == u32::MAX { 0 } else { l.col as u64 + 1 });
            put_varint(&mut v, if l.end_col == u32::MAX { 0 } else { l.end_col as u64 + 1 });
        }
        line = l.lineno as i64;
    }
    v
}

/// One decoded entry: (code units, position).
#[inline]
fn decode_entry(b: &[u8], pos: &mut usize, line: &mut i64) -> Option<(usize, Location)> {
    let first = *b.get(*pos)?;
    *pos += 1;
    let code = (first >> 3) & 15;
    let units = (first & 7) as usize + 1;
    let none = u32::MAX;
    let loc = match code {
        15 => Location { lineno: 0, end_lineno: 0, col: none, end_col: none },
        14 => {
            *line += get_svarint(b, pos)?;
            let end = *line + get_varint(b, pos)? as i64;
            let c = get_varint(b, pos)?;
            let e = get_varint(b, pos)?;
            Location {
                lineno: *line as u32,
                end_lineno: end as u32,
                col: if c == 0 { none } else { (c - 1) as u32 },
                end_col: if e == 0 { none } else { (e - 1) as u32 },
            }
        }
        13 => {
            *line += get_svarint(b, pos)?;
            Location { lineno: *line as u32, end_lineno: *line as u32, col: none, end_col: none }
        }
        10..=12 => {
            *line += (code - 10) as i64;
            let c = *b.get(*pos)? as u32;
            let e = *b.get(*pos + 1)? as u32;
            *pos += 2;
            Location { lineno: *line as u32, end_lineno: *line as u32, col: c, end_col: e }
        }
        _ => {
            let s = *b.get(*pos)?;
            *pos += 1;
            let c = (code as u32) << 3 | (s >> 4) as u32;
            Location { lineno: *line as u32, end_lineno: *line as u32, col: c, end_col: c + (s & 15) as u32 }
        }
    };
    Some((units, loc))
}

/// Decoded positions for a code object (a view of its `co_linetable`).
pub struct LineTable {
    raw: PyRef,
    n: u32,
    firstlineno: u32,
    /// `-X no_debug_ranges`: report no columns.
    strip_cols: bool,
    /// (byte offset, first code unit, line before the entry) about every LT_CHUNK
    /// instructions, built on the first random access.
    chunks: std::sync::OnceLock<Box<[(u32, u32, i64)]>>,
}

impl LineTable {
    /// Check that `raw` is a well-formed table for `ninstr` instructions.
    pub fn from_bytes(raw: PyRef, ninstr: usize, firstlineno: u32, strip_cols: bool) -> Option<LineTable> {
        let b = bytes::bytes_data(&raw);
        let (mut pos, mut line, mut units) = (0, firstlineno as i64, 0usize);
        while pos < b.len() {
            if b[pos] & 0x80 == 0 {
                return None;
            }
            units += decode_entry(b, &mut pos, &mut line)?.0;
        }
        if units != 2 * ninstr {
            return None;
        }
        Some(LineTable { raw, n: ninstr as u32, firstlineno, strip_cols, chunks: std::sync::OnceLock::new() })
    }
    pub fn from_locations(locs: &[Location], firstlineno: u32, strip_cols: bool) -> LineTable {
        let raw = bytes::new_bytes(&encode_lines(locs, firstlineno));
        LineTable::from_bytes(raw, locs.len(), firstlineno, strip_cols).expect("fresh line table")
    }
    /// The encoded table (`co_linetable`).
    pub fn raw(&self) -> &PyRef {
        &self.raw
    }
    pub fn len(&self) -> usize {
        self.n as usize
    }
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
    #[inline]
    fn adjust(&self, l: Location) -> Location {
        if self.strip_cols { Location { col: u32::MAX, end_col: u32::MAX, ..l } } else { l }
    }
    fn chunks(&self) -> &[(u32, u32, i64)] {
        self.chunks.get_or_init(|| {
            let b = bytes::bytes_data(&self.raw);
            let mut v = Vec::with_capacity(self.n as usize / LT_CHUNK + 1);
            let (mut pos, mut line, mut unit) = (0usize, self.firstlineno as i64, 0usize);
            let mut next_mark = 0;
            while pos < b.len() {
                if unit >= next_mark {
                    v.push((pos as u32, unit as u32, line));
                    next_mark = unit + 2 * LT_CHUNK;
                }
                match decode_entry(b, &mut pos, &mut line) {
                    Some((u, _)) => unit += u,
                    None => break,
                }
            }
            v.into_boxed_slice()
        })
    }
    /// Position of instruction `i`.
    pub fn get(&self, i: usize) -> Option<Location> {
        if i >= self.n as usize {
            return None;
        }
        let want = 2 * i;
        let ch = self.chunks();
        let k = ch.partition_point(|c| c.1 as usize <= want).max(1) - 1;
        let (off, unit0, line0) = *ch.get(k)?;
        let b = bytes::bytes_data(&self.raw);
        let (mut pos, mut line, mut unit) = (off as usize, line0, unit0 as usize);
        loop {
            let (u, l) = decode_entry(b, &mut pos, &mut line)?;
            if want < unit + u {
                return Some(self.adjust(l));
            }
            unit += u;
        }
    }
    pub fn iter(&self) -> LineIter<'_> {
        LineIter { t: self, pos: 0, line: self.firstlineno as i64, cur: None, left: 0, emitted: 0 }
    }
}

/// Positions of the instructions in order.
pub struct LineIter<'a> {
    t: &'a LineTable,
    pos: usize,
    line: i64,
    cur: Option<Location>,
    /// code units of `cur` not yet reported
    left: usize,
    emitted: u32,
}

impl Iterator for LineIter<'_> {
    type Item = Location;
    fn next(&mut self) -> Option<Location> {
        if self.emitted >= self.t.n {
            return None;
        }
        while self.left < 2 {
            let b = bytes::bytes_data(&self.t.raw);
            let (u, l) = decode_entry(b, &mut self.pos, &mut self.line)?;
            // a unit left over from the previous entry belongs to this instruction
            self.left += u;
            self.cur = Some(l);
        }
        self.left -= 2;
        self.emitted += 1;
        self.cur.map(|l| self.t.adjust(l))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let r = (self.t.n - self.emitted) as usize;
        (r, Some(r))
    }
}

impl<'a> IntoIterator for &'a LineTable {
    type Item = Location;
    type IntoIter = LineIter<'a>;
    fn into_iter(self) -> LineIter<'a> {
        self.iter()
    }
}

/// Convert the previous fixed-width position format (16 bytes per instruction:
/// lineno, end_lineno, col, end_col as u32 LE), still found in older cached bytecode.
pub fn lines_from_fixed(b: &[u8]) -> Vec<Location> {
    b.chunks_exact(16)
        .map(|c| {
            let w = |k: usize| u32::from_le_bytes([c[k], c[k + 1], c[k + 2], c[k + 3]]);
            Location { lineno: w(0), end_lineno: w(4), col: w(8), end_col: w(12) }
        })
        .collect()
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
        let d = code_data(o);
        let c = &*(o.as_ptr() as *const PyCodeObject);
        CodeRef { obj: c, d, co_name: &d.co_name, co_qualname: &d.co_qualname }
    }
}

#[inline(always)]
pub fn code_data(o: &PyObject) -> &CodeData {
    unsafe {
        let p = std::sync::atomic::AtomicPtr::from_ptr(&raw mut (*(o.as_ptr() as *mut PyCodeObject)).data).load(std::sync::atomic::Ordering::Acquire);
        if !p.is_null() { &*p } else { load_body(o) }
    }
}

/// Name, qualified name and docstring without loading a lazy body (function creation).
pub fn code_name(o: &PyObject) -> PyRef {
    unsafe { PyRef::from_borrowed((*(o.as_ptr() as *const PyCodeObject)).co_name) }
}
pub fn code_qualname(o: &PyObject) -> PyRef {
    unsafe { PyRef::from_borrowed((*(o.as_ptr() as *const PyCodeObject)).co_qualname) }
}
pub fn code_docstring(o: &PyObject) -> Option<PyRef> {
    unsafe {
        let c = &*(o.as_ptr() as *const PyCodeObject);
        if c.data.is_null() {
            return if c.doc.is_null() { None } else { Some(PyRef::from_borrowed(c.doc)) };
        }
    }
    code(o).docstring()
}

/// Unmarshal the body of a lazily loaded code object (once; concurrent callers wait).
#[cold]
#[inline(never)]
fn load_body(o: &PyObject) -> &CodeData {
    unsafe {
        let c = o.as_ptr() as *mut PyCodeObject;
        let _g = crate::types::dict::ob_lock(o);
        if (*c).data.is_null() {
            let lazy = (*c).lazy;
            if lazy.is_null() {
                crate::capi::refcount::fatal("code object without a body");
            }
            let saved = crate::errors::take_exc();
            match crate::modules::marshal::read_lazy_body(o, &*lazy) {
                Ok(parts) => fill_code(c, parts),
                Err(_) => {
                    crate::errors::clear_exc();
                    crate::capi::refcount::fatal("corrupt marshal data in a code object body");
                }
            }
            if let Some(e) = saved {
                crate::errors::set_exc(e);
            }
            (*c).lazy = std::ptr::null_mut();
            drop(Box::from_raw(lazy));
            let d = std::mem::replace(&mut (*c).doc, std::ptr::null_mut());
            xdecref(d);
        }
        &*(*c).data
    }
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
    let filename = match filename {
        Some(f) => f.clone(),
        None => pystr::intern_str(&cu.filename),
    };
    let mut consts = Vec::with_capacity(cu.consts.len());
    for c in &cu.consts {
        consts.push(const_to_obj(c, &filename)?);
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
        lines: Lines::Decoded(&cu.locations),
        exception_table: cu.exception_table.clone(),
        exctable_raw: None,
        kinds: cu.localspluskinds.clone(),
        filename,
        consts: tuple::new_tuple(consts),
        names: tuple::new_tuple(names),
        localsplusnames: tuple::new_tuple(lpn),
        name: pystr::intern_str(&cu.name),
        qualname: pystr::intern_str(&cu.qualname),
    };
    new_code_from_parts(parts)
}

/// Source positions handed to `new_code_from_parts`.
pub enum Lines<'a> {
    Decoded(&'a [Location]),
    /// An already encoded table (`encode_lines` format) of the right length.
    Encoded(LineTable),
}

/// Everything a code object is made of, with the object-valued parts already built
/// (used by the compiler path above and by marshal, which reads objects directly).
pub struct CodeParts<'a> {
    pub argcount: u32,
    pub posonlyargcount: u32,
    pub kwonlyargcount: u32,
    pub flags: u32,
    pub stacksize: u32,
    pub firstlineno: u32,
    pub code: Vec<u32>,
    pub lines: Lines<'a>,
    pub exception_table: Vec<ExceptionEntry>,
    /// `co_exceptiontable` if the caller already has it encoded.
    pub exctable_raw: Option<PyRef>,
    pub kinds: Vec<u8>,
    pub filename: PyRef,
    /// exact tuple
    pub consts: PyRef,
    /// exact tuple of interned names
    pub names: PyRef,
    /// exact tuple of interned names
    pub localsplusnames: PyRef,
    pub name: PyRef,
    pub qualname: PyRef,
}

pub fn new_code_from_parts(p: CodeParts) -> PyResult {
    unsafe {
        let op = crate::alloc::alloc_object(code_type(), std::mem::size_of::<PyCodeObject>()) as *mut PyCodeObject;
        fill_header(op, p.argcount, p.posonlyargcount, p.kwonlyargcount, p.flags, p.stacksize, p.firstlineno, &p.kinds);
        let c = &mut *op;
        c.co_localspluskinds = bytes::new_bytes(&p.kinds).into_raw();
        c.co_filename = p.filename.clone().into_raw();
        c.co_name = p.name.clone().into_raw();
        c.co_qualname = p.qualname.clone().into_raw();
        fill_code(op, p);
        Ok(PyRef::from_raw(op as *mut PyObject))
    }
}

/// The scalar header fields of a fresh code object.
#[allow(clippy::too_many_arguments)]
unsafe fn fill_header(op: *mut PyCodeObject, argc: u32, posonly: u32, kwonly: u32, flags: u32, stacksize: u32, firstlineno: u32, kinds: &[u8]) {
    unsafe {
        let c = &mut *op;
        c.co_flags = flags as c_int;
        c.co_argcount = argc as c_int;
        c.co_posonlyargcount = posonly as c_int;
        c.co_kwonlyargcount = kwonly as c_int;
        c.co_stacksize = stacksize as c_int;
        c.co_firstlineno = firstlineno as c_int;
        c.co_nlocalsplus = kinds.len() as c_int;
        c.co_framesize = (kinds.len() + stacksize as usize) as c_int;
        c.co_nlocals = kinds.iter().filter(|&&k| k & bc::fast::LOCAL != 0).count() as c_int;
        c.co_ncellvars = kinds.iter().filter(|&&k| k & bc::fast::CELL != 0).count() as c_int;
        c.co_nfreevars = kinds.iter().filter(|&&k| k & bc::fast::FREE != 0).count() as c_int;
        c.co_version = 1;
    }
}

/// Header fields of a code object whose body stays marshalled until first use.
pub struct LazyHeader {
    pub argcount: u32,
    pub posonlyargcount: u32,
    pub kwonlyargcount: u32,
    pub flags: u32,
    pub stacksize: u32,
    pub firstlineno: u32,
    /// `co_localspluskinds` (bytes)
    pub kinds: PyRef,
    pub filename: PyRef,
    pub name: PyRef,
    pub qualname: PyRef,
    /// docstring (or None)
    pub doc: PyRef,
}

/// A code object whose body is loaded from `body` on first use (`code_data`).
pub fn new_code_lazy(h: LazyHeader, body: LazyBody) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(code_type(), std::mem::size_of::<PyCodeObject>()) as *mut PyCodeObject;
        fill_header(op, h.argcount, h.posonlyargcount, h.kwonlyargcount, h.flags, h.stacksize, h.firstlineno, bytes::bytes_data(&h.kinds));
        let c = &mut *op;
        c.co_localspluskinds = h.kinds.into_raw();
        c.co_filename = h.filename.into_raw();
        c.co_name = h.name.into_raw();
        c.co_qualname = h.qualname.into_raw();
        if !h.doc.is(none::none()) {
            c.doc = h.doc.into_raw();
        }
        c.lazy = Box::into_raw(Box::new(body));
        PyRef::from_raw(op as *mut PyObject)
    }
}

/// Install the body (`data` and the object-valued fields) of a code object whose
/// header is set. The data pointer is published last.
unsafe fn fill_code(op: *mut PyCodeObject, p: CodeParts) {
    let kinds = p.kinds;
    let cell_slots: Vec<u32> =
        kinds.iter().enumerate().filter(|(_, k)| **k & bc::fast::CELL != 0).map(|(i, _)| i as u32).collect();
    let exctable = match p.exctable_raw {
        Some(b) => b,
        None => bytes::new_bytes(&encode_exception_table(&p.exception_table)),
    };
    let lines = match p.lines {
        Lines::Encoded(t) => t,
        Lines::Decoded(locs) => LineTable::from_locations(locs, p.firstlineno, false),
    };
    let linetable = lines.raw().clone();
    let ninstr = p.code.len();
    let mut code = p.code;
    code.shrink_to_fit();
    let data = Box::new(CodeData {
        code,
        locations: lines,
        exception_table: p.exception_table,
        consts: RefTuple::new(p.consts.clone()),
        names: RefTuple::new(p.names.clone()),
        localsplusnames: RefTuple::new(p.localsplusnames.clone()),
        localspluskinds: kinds,
        co_name: p.name,
        co_qualname: p.qualname,
        filename: p.filename,
        cell_slots,
        invocation_count: std::sync::atomic::AtomicU32::new(0),
        backedge_count: std::sync::atomic::AtomicU32::new(0),
        caches: crate::interp::cache::CodeCaches::new(ninstr),
    });
    unsafe {
        let c = &mut *op;
        c.co_consts = p.consts.into_raw();
        c.co_names = p.names.into_raw();
        c.co_exceptiontable = exctable.into_raw();
        c.co_localsplusnames = p.localsplusnames.into_raw();
        c.co_linetable = linetable.into_raw();
        std::sync::atomic::AtomicPtr::from_ptr(&raw mut c.data).store(Box::into_raw(data), std::sync::atomic::Ordering::Release);
    }
}

pub fn encode_exception_table(t: &[ExceptionEntry]) -> Vec<u8> {
    let mut v = Vec::with_capacity(t.len() * 20);
    for e in t {
        for x in [e.start, e.end, e.target, e.depth, e.lasti as u32] {
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
            c.doc,
        ] {
            xdecref(p);
        }
        if !c.data.is_null() {
            drop(Box::from_raw(c.data));
        }
        if !c.lazy.is_null() {
            drop(Box::from_raw(c.lazy));
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
            let dx = code_data(&*a);
            let dy = code_data(&*b);
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
        let d = code_data(&*op);
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
    let d = code_data(o);
    let mut v = Vec::with_capacity(d.code.len() * 4);
    for w in &d.code {
        v.extend_from_slice(&w.to_le_bytes());
    }
    Ok(bytes::new_bytes(&v))
}
fn co_lines(o: &PyObject) -> PyResult {
    // (start_offset, end_offset, lineno) with offsets in bytes (4 per instruction)
    let d = code_data(o);
    let lines: Vec<u32> = d.locations.iter().map(|l| l.lineno).collect();
    let mut out: Vec<PyRef> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let mut j = i + 1;
        while j < lines.len() && lines[j] == line {
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
    code_data(o);
    borrowed(get(o).co_consts)
}
fn co_names_get(o: &PyObject) -> PyResult {
    code_data(o);
    borrowed(get(o).co_names)
}
fn co_linetable_get(o: &PyObject) -> PyResult {
    code_data(o);
    borrowed(get(o).co_linetable)
}
fn co_exceptiontable_get(o: &PyObject) -> PyResult {
    code_data(o);
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
        locations: d.locations.iter().collect(),
        consts: Vec::new(),
        names: d.names.iter().map(|n| pystr::to_string_lossy(n)).collect(),
        localsplusnames: d.localsplusnames.iter().map(|n| pystr::to_string_lossy(n)).collect(),
        localspluskinds: d.localspluskinds.clone(),
        exception_table: d.exception_table.clone(),
    };
    let mut new_consts: Option<PyRef> = None;
    let mut new_names: Option<PyRef> = None;
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
            "co_varnames" | "co_freevars" | "co_cellvars" | "co_nlocals" | "co_code" | "co_linetable"
            | "co_exceptiontable" => {
                raise!(ValueError, "replace(): changing {} is not supported by this implementation", ks)
            }
            _ => raise!(TypeError, "replace() got an unexpected keyword argument '{}'", ks),
        }
    }
    let r = new_code_from_unit(&cu, None)?;
    unsafe {
        let rc = &mut *(r.as_ptr() as *mut PyCodeObject);
        let rd = &mut *rc.data;
        let consts = match new_consts {
            Some(c) => crate::abstract_::sequence_tuple(&c)?,
            None => PyRef::from_borrowed(c.co_consts),
        };
        rd.consts = RefTuple::from_vec(tuple::tuple_to_vec(&consts));
        let old = rc.co_consts;
        rc.co_consts = consts.into_raw();
        xdecref(old);
        if let Some(n) = new_names {
            let t = crate::abstract_::sequence_tuple(&n)?;
            rd.names = RefTuple::from_vec(tuple::tuple_to_vec(&t));
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
