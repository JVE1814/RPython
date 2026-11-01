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
    new_code_from_parts(parts)
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
fn co_positions(o: &PyObject) -> PyResult {
    let d = code_data(o);
    let mut out: Vec<PyRef> = Vec::new();
    let opt = |x: u32, none_if_zero: bool| -> PyRef {
        if (none_if_zero && x == 0) || x == u32::MAX { none::none_ref() } else { int::new_int(x as i64) }
    };
    for l in &d.locations {
        let item = tuple::new_tuple(vec![
            opt(l.lineno, true),
            opt(l.end_lineno, true),
            if l.lineno == 0 { none::none_ref() } else { opt(l.col, false) },
            if l.lineno == 0 { none::none_ref() } else { opt(l.end_col, false) },
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
            ("replace", FASTKW, code_replace),
            ("__replace__", FASTKW, code_replace),
            ("__sizeof__", NOARGS, code_sizeof),
        ];
    }
    let _ = std::ptr::null::<c_void>();
}
