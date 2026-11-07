//! The `marshal` module: CPython's marshal format, versions 0..=6.
//!
//! Type codes, FLAG_REF back-references, ints with 15-bit digits, float/complex in text
//! (version < 2) or binary form, compact ASCII strings (version >= 4), small tuples
//! (version >= 4), slices (version >= 5) and frozendicts (version >= 6) follow CPython
//! byte for byte. Code objects use CPython 3.15's layout as well (D24/D25: the public form of
//! a code object is CPython wordcode), so .pyc files are interchangeable with CPython's:
//!   'c' argcount posonlyargcount kwonlyargcount stacksize flags     (5 x int32)
//!       co_code (bytes: wordcode, caches zeroed), co_consts, co_names, co_localsplusnames,
//!       co_localspluskinds (bytes, CO_FAST_* per name), co_filename, co_name, co_qualname,
//!       co_firstlineno (int32), co_linetable, co_exceptiontable (both bytes)

use crate::abstract_ as ab;
use crate::args::Params;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::code::{self as code_mod, PyCodeObject};
use crate::types::typeobject::tname;
use crate::types::{
    bool_, bytearray, bytes, complex, dict, exceptions, float, int, list, none, set, slice, str as pystr, tuple,
};
use crate::{mdefs, raise};
use rpy_compiler::bytecode as bc;
use std::collections::HashMap;

pub const VERSION: i32 = 6;
const MAX_DEPTH: usize = 2000;
const SIZE32_MAX: i64 = 0x7fff_ffff;

const TYPE_NULL: u8 = b'0';
const TYPE_NONE: u8 = b'N';
const TYPE_FALSE: u8 = b'F';
const TYPE_TRUE: u8 = b'T';
const TYPE_STOPITER: u8 = b'S';
const TYPE_ELLIPSIS: u8 = b'.';
const TYPE_INT: u8 = b'i';
const TYPE_INT64: u8 = b'I';
const TYPE_FLOAT: u8 = b'f';
const TYPE_BINARY_FLOAT: u8 = b'g';
const TYPE_COMPLEX: u8 = b'x';
const TYPE_BINARY_COMPLEX: u8 = b'y';
const TYPE_LONG: u8 = b'l';
const TYPE_STRING: u8 = b's';
const TYPE_INTERNED: u8 = b't';
const TYPE_REF: u8 = b'r';
const TYPE_TUPLE: u8 = b'(';
const TYPE_LIST: u8 = b'[';
const TYPE_DICT: u8 = b'{';
const TYPE_CODE: u8 = b'c';
const TYPE_UNICODE: u8 = b'u';
const TYPE_UNKNOWN: u8 = b'?';
const TYPE_SET: u8 = b'<';
const TYPE_FROZENSET: u8 = b'>';
const TYPE_ASCII: u8 = b'a';
const TYPE_ASCII_INTERNED: u8 = b'A';
const TYPE_SMALL_TUPLE: u8 = b')';
const TYPE_SHORT_ASCII: u8 = b'z';
const TYPE_SHORT_ASCII_INTERNED: u8 = b'Z';
const TYPE_SLICE: u8 = b':';
const TYPE_FROZENDICT: u8 = b'}';
const FLAG_REF: u8 = 0x80;

fn unmarshallable() -> PyErr {
    value_error("unmarshallable object")
}

/// The builtin `frozendict` type if the runtime provides one.
fn frozendict_type() -> Option<PyRef> {
    let b = crate::builtins::builtins_dict();
    dict::get_item_rs(&b, "frozendict").filter(|t| crate::types::typeobject::is_type(t))
}

fn is_frozendict_exact(o: &PyObject) -> bool {
    match frozendict_type() {
        Some(t) => o.type_ptr() as *mut PyObject == t.as_ptr(),
        None => false,
    }
}

// ====================================================================== writer

struct Writer {
    out: Vec<u8>,
    version: i32,
    allow_code: bool,
    depth: usize,
    /// object address -> (reference index, complete)
    refs: HashMap<usize, (u32, bool)>,
    /// keeps every registered object alive so addresses stay unique
    keep: Vec<PyRef>,
}

impl Writer {
    fn new(version: i32, allow_code: bool, depth: usize) -> Writer {
        Writer { out: Vec::new(), version, allow_code, depth, refs: HashMap::new(), keep: Vec::new() }
    }
    #[inline]
    fn byte(&mut self, b: u8) {
        self.out.push(b);
    }
    #[inline]
    fn long(&mut self, v: i32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn size(&mut self, n: usize) -> PyResult<()> {
        if n as i64 > SIZE32_MAX {
            return Err(unmarshallable());
        }
        self.long(n as i32);
        Ok(())
    }
    fn pstring(&mut self, b: &[u8]) -> PyResult<()> {
        self.size(b.len())?;
        self.out.extend_from_slice(b);
        Ok(())
    }
    fn float_bin(&mut self, v: f64) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn float_str(&mut self, v: f64) {
        let mut s = String::new();
        let _ = rpy_numfmt::double_to_string(&mut s, v, 'g', 17, rpy_numfmt::DtoaFlags::default());
        self.byte(s.len() as u8);
        self.out.extend_from_slice(s.as_bytes());
    }

    fn object(&mut self, v: &PyObject) -> PyResult<()> {
        self.depth += 1;
        let r = self.object_inner(v);
        self.depth -= 1;
        r
    }

    fn object_inner(&mut self, v: &PyObject) -> PyResult<()> {
        if self.depth > MAX_DEPTH {
            raise!(ValueError, "object too deeply nested to marshal");
        }
        if v.is(none::none()) {
            self.byte(TYPE_NONE);
        } else if v.as_ptr() == exceptions::StopIteration() as *mut PyObject {
            self.byte(TYPE_STOPITER);
        } else if v.is(none::ellipsis()) {
            self.byte(TYPE_ELLIPSIS);
        } else if v.is(bool_::false_obj()) {
            self.byte(TYPE_FALSE);
        } else if v.is(bool_::true_obj()) {
            self.byte(TYPE_TRUE);
        } else {
            self.complex_object(v)?;
        }
        Ok(())
    }

    /// Returns (flag, registered-index) or writes a back-reference and returns None.
    fn w_ref(&mut self, v: &PyObject, complete: bool) -> PyResult<Option<(u8, Option<usize>)>> {
        if self.version < 3 {
            return Ok(Some((0, None)));
        }
        let key = v.as_ptr() as usize;
        if let Some(&(idx, done)) = self.refs.get(&key) {
            if done {
                self.byte(TYPE_REF);
                self.long(idx as i32);
                return Ok(None);
            }
            // An object under construction referring to itself. Code objects, slices and
            // frozendicts can only be built once their parts exist: refuse the loop.
            if code_mod::is_code(v) || v.is_type(crate::types::slice::slice_type()) || crate::types::frozendict::is_frozendict(v) {
                raise!(ValueError, "cannot marshal a recursive {} object", tname(v));
            }
            // Other immutable containers: write it again (recurses until the depth limit).
            return Ok(Some((0, None)));
        }
        let n = self.refs.len();
        if n >= 0x7fff_ffff {
            raise!(ValueError, "too many objects");
        }
        self.refs.insert(key, (n as u32, complete));
        self.keep.push(v.to_ref());
        Ok(Some((FLAG_REF, Some(key))))
    }

    fn mark_complete(&mut self, key: Option<usize>) {
        if let Some(k) = key {
            if let Some(e) = self.refs.get_mut(&k) {
                e.1 = true;
            }
        }
    }

    fn complex_object(&mut self, v: &PyObject) -> PyResult<()> {
        // immutable containers whose reader cannot resolve self references
        let late = code_mod::is_code(v)
            || v.is_type(set::frozenset_type())
            || slice::is_slice(v)
            || is_frozendict_exact(v);
        let (flag, key) = match self.w_ref(v, !late)? {
            None => return Ok(()),
            Some(x) => x,
        };
        let start = self.out.len();
        let r = self.complex_body(v, flag);
        if late {
            self.mark_complete(key);
        }
        if r.is_err() && start < self.out.len() {
            // keep output consistent for error reporting (not used afterwards)
        }
        r
    }

    fn complex_body(&mut self, v: &PyObject, flag: u8) -> PyResult<()> {
        if int::is_int_exact(v) {
            if let Some(x) = int::as_i64(v) {
                if x >= i32::MIN as i64 && x <= i32::MAX as i64 {
                    self.byte(TYPE_INT | flag);
                    self.long(x as i32);
                    return Ok(());
                }
            }
            self.byte(TYPE_LONG | flag);
            return self.pylong(v);
        }
        if float::is_float_exact(v) {
            let x = float::float_value(v);
            if self.version > 1 {
                self.byte(TYPE_BINARY_FLOAT | flag);
                self.float_bin(x);
            } else {
                self.byte(TYPE_FLOAT | flag);
                self.float_str(x);
            }
            return Ok(());
        }
        if complex::is_complex_exact(v) {
            let c = complex::complex_value(v);
            if self.version > 1 {
                self.byte(TYPE_BINARY_COMPLEX | flag);
                self.float_bin(c.real);
                self.float_bin(c.imag);
            } else {
                self.byte(TYPE_COMPLEX | flag);
                self.float_str(c.real);
                self.float_str(c.imag);
            }
            return Ok(());
        }
        if bytes::is_bytes_exact(v) {
            self.byte(TYPE_STRING | flag);
            return self.pstring(bytes::bytes_data(v));
        }
        if pystr::is_str_exact(v) {
            let interned = pystr::is_interned(v);
            if self.version >= 4 && pystr::str_is_ascii(v) {
                let n = pystr::str_len(v);
                let data = unsafe { std::slice::from_raw_parts(pystr::str_data_ptr(v), n) };
                if n < 256 {
                    self.byte(if interned { TYPE_SHORT_ASCII_INTERNED } else { TYPE_SHORT_ASCII } | flag);
                    self.byte(n as u8);
                    self.out.extend_from_slice(data);
                } else {
                    self.byte(if interned { TYPE_ASCII_INTERNED } else { TYPE_ASCII } | flag);
                    self.pstring(data)?;
                }
            } else {
                let utf8 = encode_utf8_surrogatepass(v);
                self.byte(if self.version >= 3 && interned { TYPE_INTERNED } else { TYPE_UNICODE } | flag);
                self.pstring(&utf8)?;
            }
            return Ok(());
        }
        if tuple::is_tuple_exact(v) {
            let items = tuple::tuple_items(v);
            let n = items.len();
            if self.version >= 4 && n < 256 {
                self.byte(TYPE_SMALL_TUPLE | flag);
                self.byte(n as u8);
            } else {
                self.byte(TYPE_TUPLE | flag);
                self.size(n)?;
            }
            for it in items {
                self.object(it)?;
            }
            return Ok(());
        }
        if list::is_list_exact(v) {
            self.byte(TYPE_LIST | flag);
            let n = list::list_len(v);
            self.size(n)?;
            let mut i = 0;
            while i < n {
                // the list may be mutated by __buffer__ of an item: re-read each time
                match list::get(v, i) {
                    Some(it) => self.object(&it)?,
                    None => self.byte(TYPE_NULL),
                }
                i += 1;
            }
            return Ok(());
        }
        if dict::is_dict_exact(v) {
            self.byte(TYPE_DICT | flag);
            self.dict_items(v)?;
            return Ok(());
        }
        if v.is_type(set::set_type()) || v.is_type(set::frozenset_type()) {
            self.byte(if v.is_type(set::set_type()) { TYPE_SET } else { TYPE_FROZENSET } | flag);
            return self.set_items(v);
        }
        if code_mod::is_code(v) {
            if !self.allow_code {
                raise!(ValueError, "unmarshalling code objects is disallowed");
            }
            self.byte(TYPE_CODE | flag);
            return self.code(v);
        }
        if crate::buffer::check_buffer(v) {
            let data = crate::buffer::with_buffer(v, |b| b.to_vec())?;
            self.byte(TYPE_STRING | flag);
            return self.pstring(&data);
        }
        if slice::is_slice(v) {
            if self.version < 5 {
                self.byte(TYPE_UNKNOWN);
                return Err(unmarshallable());
            }
            self.byte(TYPE_SLICE | flag);
            let (a, b, c) = slice::parts(v);
            let (a, b, c) = (a.to_ref(), b.to_ref(), c.to_ref());
            self.object(&a)?;
            self.object(&b)?;
            return self.object(&c);
        }
        if is_frozendict_exact(v) {
            if self.version < 6 {
                self.byte(TYPE_UNKNOWN);
                return Err(unmarshallable());
            }
            self.byte(TYPE_FROZENDICT | flag);
            return self.frozendict_items(v);
        }
        self.byte(TYPE_UNKNOWN);
        Err(unmarshallable())
    }

    fn dict_items(&mut self, d: &PyObject) -> PyResult<()> {
        let mut pos = 0usize;
        while let Some((k, val)) = dict::next_entry(d, &mut pos) {
            self.object(&k)?;
            self.object(&val)?;
        }
        self.byte(TYPE_NULL);
        Ok(())
    }

    fn frozendict_items(&mut self, d: &PyObject) -> PyResult<()> {
        let items = ab::call_method_str(d, "items", &[])?;
        let it = ab::get_iter(&items)?;
        while let Some(pair) = ab::iter_next(&it)? {
            let k = ab::getitem(&pair, &int::new_int(0))?;
            let val = ab::getitem(&pair, &int::new_int(1))?;
            self.object(&k)?;
            self.object(&val)?;
        }
        self.byte(TYPE_NULL);
        Ok(())
    }

    fn set_items(&mut self, s: &PyObject) -> PyResult<()> {
        let n = set::set_len(s);
        self.size(n)?;
        let items = set::set_items(s);
        // Deterministic output: order the elements by their own marshalled form.
        let mut pairs: Vec<(Vec<u8>, PyRef)> = Vec::with_capacity(items.len());
        for it in items {
            let mut w = Writer::new(self.version, self.allow_code, self.depth);
            w.object(&it)?;
            if set::set_len(s) != n {
                raise!(RuntimeError, "set changed size during iteration");
            }
            pairs.push((w.out, it));
        }
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, it) in pairs {
            self.object(&it)?;
        }
        Ok(())
    }

    fn pylong(&mut self, v: &PyObject) -> PyResult<()> {
        let neg = int::sign(v) < 0;
        let d30 = int::digits(v);
        let d15 = digits30_to_15(d30);
        let n = d15.len();
        if n as i64 > SIZE32_MAX {
            return Err(unmarshallable());
        }
        self.long(if neg { -(n as i32) } else { n as i32 });
        for d in d15 {
            self.out.extend_from_slice(&d.to_le_bytes());
        }
        Ok(())
    }

    fn code(&mut self, v: &PyObject) -> PyResult<()> {
        let c = unsafe { &*(v.as_ptr() as *const PyCodeObject) };
        let m = code_mod::code_meta(v);
        self.long(c.co_argcount);
        self.long(c.co_posonlyargcount);
        self.long(c.co_kwonlyargcount);
        self.long(c.co_stacksize);
        self.long(c.co_flags);
        self.object(&m.co_code)?;
        unsafe {
            self.object(&*c.co_consts)?;
            self.object(&*c.co_names)?;
            self.object(&*c.co_localsplusnames)?;
            self.object(&*c.co_localspluskinds)?;
            self.object(&*c.co_filename)?;
            self.object(&*c.co_name)?;
            self.object(&*c.co_qualname)?;
            self.long(c.co_firstlineno);
            self.object(&*c.co_linetable)?;
            self.object(&*c.co_exceptiontable)?;
        }
        Ok(())
    }
}

fn digits30_to_15(d30: &[u32]) -> Vec<u16> {
    let mut out = Vec::with_capacity(d30.len() * 2);
    for &d in d30 {
        out.push((d & 0x7fff) as u16);
        out.push(((d >> 15) & 0x7fff) as u16);
    }
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

fn digits15_to_30(d15: &[u16]) -> Vec<u32> {
    let mut out = Vec::with_capacity(d15.len().div_ceil(2));
    for ch in d15.chunks(2) {
        let lo = ch[0] as u32;
        let hi = if ch.len() > 1 { ch[1] as u32 } else { 0 };
        out.push(lo | (hi << 15));
    }
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

fn encode_utf8_surrogatepass(s: &PyObject) -> Vec<u8> {
    if let Some(a) = pystr::ascii_str(s) {
        return a.as_bytes().to_vec();
    }
    let cps = pystr::str_data(s).to_vec();
    let mut out = Vec::with_capacity(cps.len() * 2);
    for c in cps {
        match c {
            0..=0x7f => out.push(c as u8),
            0x80..=0x7ff => {
                out.push(0xc0 | (c >> 6) as u8);
                out.push(0x80 | (c & 0x3f) as u8);
            }
            0x800..=0xffff => {
                out.push(0xe0 | (c >> 12) as u8);
                out.push(0x80 | ((c >> 6) & 0x3f) as u8);
                out.push(0x80 | (c & 0x3f) as u8);
            }
            _ => {
                out.push(0xf0 | (c >> 18) as u8);
                out.push(0x80 | ((c >> 12) & 0x3f) as u8);
                out.push(0x80 | ((c >> 6) & 0x3f) as u8);
                out.push(0x80 | (c & 0x3f) as u8);
            }
        }
    }
    out
}

/// PyMarshal_WriteObjectToString equivalent.
pub fn dumps_vec(v: &PyObject, version: i32, allow_code: bool) -> PyResult<Vec<u8>> {
    let mut w = Writer::new(version, allow_code, 0);
    w.object(v)?;
    Ok(w.out)
}

// ====================================================================== reader

enum Source<'a> {
    Mem { data: &'a [u8], pos: usize },
    File { file: &'a PyObject, consumed: usize },
}

struct Reader<'a> {
    src: Source<'a>,
    /// back-reference table; None = reserved (object under construction)
    refs: Vec<Option<PyRef>>,
    depth: usize,
    allow_code: bool,
}

fn bad(what: &str) -> PyErr {
    value_error(&format!("bad marshal data ({})", what))
}

impl<'a> Reader<'a> {
    fn read(&mut self, n: usize) -> PyResult<std::borrow::Cow<'a, [u8]>> {
        match &mut self.src {
            Source::Mem { data, pos } => {
                if data.len() - *pos < n {
                    raise!(EOFError, "marshal data too short");
                }
                let s = &data[*pos..*pos + n];
                *pos += n;
                Ok(std::borrow::Cow::Borrowed(s))
            }
            Source::File { file, consumed } => {
                if n == 0 {
                    return Ok(std::borrow::Cow::Owned(Vec::new()));
                }
                let buf = bytearray::new_bytearray(&crate::errors::try_zeroed_vec(n)?)?;
                let r = ab::call_method_str(file, "readinto", &[&buf])?;
                let got = if r.is(none::none()) { 0 } else { ab::index_as_ssize(&r, None)? };
                if got != n as isize {
                    if got > n as isize {
                        raise!(ValueError, "read() returned too much data: {} bytes requested, {} returned", n, got);
                    }
                    raise!(EOFError, "EOF read where not expected");
                }
                *consumed += n;
                Ok(std::borrow::Cow::Owned(bytearray::bytearray_data(&buf).to_vec()))
            }
        }
    }

    /// Next type byte; None at end of input.
    fn type_byte(&mut self) -> PyResult<Option<u8>> {
        match &mut self.src {
            Source::Mem { data, pos } => {
                if *pos < data.len() {
                    let b = data[*pos];
                    *pos += 1;
                    Ok(Some(b))
                } else {
                    Ok(None)
                }
            }
            Source::File { .. } => match self.read(1) {
                Ok(b) => Ok(Some(b[0])),
                Err(e) => {
                    if exc_matches(exceptions::EOFError()) {
                        clear_exc();
                        Ok(None)
                    } else {
                        Err(e)
                    }
                }
            },
        }
    }

    fn byte(&mut self) -> PyResult<u8> {
        Ok(self.read(1)?[0])
    }
    fn long(&mut self) -> PyResult<i32> {
        let b = self.read(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn float_bin(&mut self) -> PyResult<f64> {
        let b = self.read(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(&b);
        Ok(f64::from_le_bytes(a))
    }
    fn float_str(&mut self) -> PyResult<f64> {
        let n = self.byte()? as usize;
        let b = self.read(n)?;
        match rpy_numfmt::parse_float_bytes(&b) {
            Ok(f) => Ok(f),
            Err(_) => {
                let s = String::from_utf8_lossy(&b).into_owned();
                raise!(ValueError, "could not convert string to float: '{}'", s)
            }
        }
    }

    fn reserve(&mut self, flag: bool) -> Option<usize> {
        if flag {
            self.refs.push(None);
            Some(self.refs.len() - 1)
        } else {
            None
        }
    }
    fn fill(&mut self, idx: Option<usize>, v: &PyRef) {
        if let Some(i) = idx {
            self.refs[i] = Some(v.clone());
        }
    }
    fn set_ref(&mut self, flag: bool, v: PyRef) -> PyRef {
        if flag {
            self.refs.push(Some(v.clone()));
        }
        v
    }

    /// r_object: Ok(None) means TYPE_NULL.
    fn object(&mut self) -> PyResult<Option<PyRef>> {
        self.depth += 1;
        let r = if self.depth > MAX_DEPTH {
            Err(value_error("recursion limit exceeded"))
        } else {
            self.object_inner()
        };
        self.depth -= 1;
        r
    }

    fn object_notnull(&mut self, what: &str) -> PyResult<PyRef> {
        match self.object()? {
            Some(v) => Ok(v),
            None => raise!(TypeError, "NULL object in marshal data for {}", what),
        }
    }

    fn object_inner(&mut self) -> PyResult<Option<PyRef>> {
        let code = match self.type_byte()? {
            None => raise!(EOFError, "EOF read where object expected"),
            Some(c) => c,
        };
        let flag = code & FLAG_REF != 0;
        let ty = code & !FLAG_REF;
        let v: PyRef = match ty {
            TYPE_NULL => return Ok(None),
            TYPE_NONE => none::none_ref(),
            TYPE_STOPITER => crate::types::typeobject::type_ref(exceptions::StopIteration()),
            TYPE_ELLIPSIS => none::ellipsis().to_ref(),
            TYPE_FALSE => bool_::new_bool(false),
            TYPE_TRUE => bool_::new_bool(true),
            TYPE_INT => {
                let x = self.long()?;
                self.set_ref(flag, int::new_int(x as i64))
            }
            TYPE_INT64 => {
                let b = self.read(8)?;
                let mut a = [0u8; 8];
                a.copy_from_slice(&b);
                self.set_ref(flag, int::new_int(i64::from_le_bytes(a)))
            }
            TYPE_LONG => {
                let n = self.long()? as i64;
                if n < -SIZE32_MAX || n > SIZE32_MAX {
                    return Err(bad("long size out of range"));
                }
                let size = n.unsigned_abs() as usize;
                let raw = self.read(size * 2)?;
                let mut d15 = Vec::with_capacity(size);
                for i in 0..size {
                    let d = u16::from_le_bytes([raw[2 * i], raw[2 * i + 1]]);
                    if d >= 0x8000 {
                        return Err(bad("digit out of range in long"));
                    }
                    d15.push(d);
                }
                if size > 0 && d15[size - 1] == 0 {
                    return Err(bad("unnormalized long data"));
                }
                let d30 = digits15_to_30(&d15);
                let v = if d30.is_empty() { int::new_int(0) } else { int::from_digits(n < 0, &d30) };
                self.set_ref(flag, v)
            }
            TYPE_FLOAT => {
                let x = self.float_str()?;
                self.set_ref(flag, float::new_float(x))
            }
            TYPE_BINARY_FLOAT => {
                let x = self.float_bin()?;
                self.set_ref(flag, float::new_float(x))
            }
            TYPE_COMPLEX => {
                let re = self.float_str()?;
                let im = self.float_str()?;
                self.set_ref(flag, complex::new_complex(re, im))
            }
            TYPE_BINARY_COMPLEX => {
                let re = self.float_bin()?;
                let im = self.float_bin()?;
                self.set_ref(flag, complex::new_complex(re, im))
            }
            TYPE_STRING => {
                let n = self.long()? as i64;
                if n < 0 || n > SIZE32_MAX {
                    return Err(bad("bytes object size out of range"));
                }
                let b = self.read(n as usize)?;
                self.set_ref(flag, bytes::bytes_singleton(&b).unwrap_or_else(|| bytes::new_bytes(&b)))
            }
            TYPE_ASCII | TYPE_ASCII_INTERNED | TYPE_SHORT_ASCII | TYPE_SHORT_ASCII_INTERNED => {
                let n = if ty == TYPE_ASCII || ty == TYPE_ASCII_INTERNED {
                    let n = self.long()? as i64;
                    if n < 0 || n > SIZE32_MAX {
                        return Err(bad("string size out of range"));
                    }
                    n as usize
                } else {
                    self.byte()? as usize
                };
                let b = self.read(n)?;
                let mut s = pystr::from_latin1(&b);
                if ty == TYPE_ASCII_INTERNED || ty == TYPE_SHORT_ASCII_INTERNED {
                    s = pystr::intern(s);
                }
                self.set_ref(flag, s)
            }
            TYPE_UNICODE | TYPE_INTERNED => {
                let n = self.long()? as i64;
                if n < 0 || n > SIZE32_MAX {
                    return Err(bad("string size out of range"));
                }
                let b = self.read(n as usize)?;
                let mut s = if b.is_empty() {
                    pystr::new_str("")
                } else {
                    crate::modules::codecs_core::decode_utf8(&b, "surrogatepass")?
                };
                if ty == TYPE_INTERNED {
                    s = pystr::intern(s);
                }
                self.set_ref(flag, s)
            }
            TYPE_SMALL_TUPLE | TYPE_TUPLE => {
                let n = if ty == TYPE_SMALL_TUPLE {
                    self.byte()? as usize
                } else {
                    let n = self.long()? as i64;
                    if n < 0 || n > SIZE32_MAX {
                        return Err(bad("tuple size out of range"));
                    }
                    n as usize
                };
                if n == 0 {
                    self.set_ref(flag, tuple::empty_tuple())
                } else {
                    let t = unsafe { PyRef::from_raw(tuple::alloc_tuple_of(tuple::tuple_type(), n)) };
                    let t = self.set_ref(flag, t);
                    for i in 0..n {
                        let it = self.object_notnull("tuple")?;
                        unsafe { *tuple::item_ptr(t.as_ptr()).add(i) = it.into_raw() };
                    }
                    t
                }
            }
            TYPE_LIST => {
                let n = self.long()? as i64;
                if n < 0 || n > SIZE32_MAX {
                    return Err(bad("list size out of range"));
                }
                let l = self.set_ref(flag, list::new_list_empty());
                for _ in 0..n {
                    let it = self.object_notnull("list")?;
                    list::append(&l, it);
                }
                l
            }
            TYPE_DICT => {
                let d = self.set_ref(flag, dict::new_dict());
                self.read_dict_items(&d)?;
                d
            }
            TYPE_FROZENDICT => {
                let idx = self.reserve(flag);
                let d = dict::new_dict();
                self.read_dict_items(&d)?;
                let fd = match frozendict_type() {
                    Some(t) => ab::call(&t, &[&d])?,
                    None => return Err(bad("unknown type code")),
                };
                self.fill(idx, &fd);
                fd
            }
            TYPE_SET | TYPE_FROZENSET => {
                let n = self.long()? as i64;
                if n < 0 || n > SIZE32_MAX {
                    return Err(bad("set size out of range"));
                }
                if ty == TYPE_SET {
                    let s = self.set_ref(flag, set::new_set_from_vec(Vec::new())?);
                    for _ in 0..n {
                        let it = self.object_notnull("set")?;
                        set::set_add(&s, &it)?;
                    }
                    s
                } else {
                    let idx = self.reserve(flag);
                    let mut items = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        items.push(self.object_notnull("set")?);
                    }
                    let s = set::new_frozenset_from_vec(items)?;
                    self.fill(idx, &s);
                    s
                }
            }
            TYPE_SLICE => {
                let idx = self.reserve(flag);
                let a = self.object_notnull("slice")?;
                let b = self.object_notnull("slice")?;
                let c = self.object_notnull("slice")?;
                let s = slice::new_slice(&a, &b, &c);
                self.fill(idx, &s);
                s
            }
            TYPE_CODE => {
                if !self.allow_code {
                    raise!(ValueError, "unmarshalling code objects is disallowed");
                }
                let idx = self.reserve(flag);
                let c = self.read_code()?;
                self.fill(idx, &c);
                c
            }
            TYPE_REF => {
                let n = self.long()?;
                if n < 0 || n as usize >= self.refs.len() {
                    return Err(bad("invalid reference"));
                }
                match &self.refs[n as usize] {
                    Some(v) => v.clone(),
                    None => return Err(bad("invalid reference")),
                }
            }
            _ => return Err(bad("unknown type code")),
        };
        Ok(Some(v))
    }

    fn read_dict_items(&mut self, d: &PyObject) -> PyResult<()> {
        loop {
            let k = match self.object()? {
                None => break,
                Some(k) => k,
            };
            let val = match self.object()? {
                None => break,
                Some(v) => v,
            };
            dict::set_item(d, &k, &val)?;
        }
        Ok(())
    }

    fn read_code(&mut self) -> PyResult<PyRef> {
        let argcount = self.long()?;
        let posonly = self.long()?;
        let kwonly = self.long()?;
        let stacksize = self.long()?;
        let flags = self.long()?;
        let code = self.object_notnull("object")?;
        let consts = self.object_notnull("object")?;
        let names = self.object_notnull("object")?;
        let lpnames = self.object_notnull("object")?;
        let lpkinds = self.object_notnull("object")?;
        let filename = self.object_notnull("object")?;
        let name = self.object_notnull("object")?;
        let qualname = self.object_notnull("object")?;
        let firstlineno = self.long()?;
        let linetable = self.object_notnull("object")?;
        let exctable = self.object_notnull("object")?;
        build_code(CodeParts {
            argcount,
            posonly,
            kwonly,
            stacksize,
            flags,
            code: &code,
            consts: &consts,
            names: &names,
            lpnames: &lpnames,
            lpkinds: &lpkinds,
            filename: &filename,
            name: &name,
            qualname: &qualname,
            firstlineno,
            linetable: &linetable,
            exctable: &exctable,
        })
    }
}

struct CodeParts<'a> {
    argcount: i32,
    posonly: i32,
    kwonly: i32,
    stacksize: i32,
    flags: i32,
    code: &'a PyObject,
    consts: &'a PyObject,
    names: &'a PyObject,
    lpnames: &'a PyObject,
    lpkinds: &'a PyObject,
    filename: &'a PyObject,
    name: &'a PyObject,
    qualname: &'a PyObject,
    firstlineno: i32,
    linetable: &'a PyObject,
    exctable: &'a PyObject,
}

/// Number of items if `o` is a tuple of str, else None.
fn str_count(o: &PyObject) -> Option<usize> {
    if !tuple::is_tuple(o) {
        return None;
    }
    let items = tuple::tuple_items(o);
    if items.iter().all(|it| pystr::is_str(it)) { Some(items.len()) } else { None }
}

/// `no_debug_ranges` is fixed at startup (-X / environment): look it up once.
fn no_debug_ranges_cached() -> bool {
    // _loads_without_positions asks for it per call, whatever the startup setting
    if STRIP_COLUMNS.with(|c| c.get()) {
        return true;
    }
    static V: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
    match V.load(std::sync::atomic::Ordering::Relaxed) {
        2 => {
            let v = no_debug_ranges();
            // sys may not be fully set up yet during bootstrap: only memoise once it is
            if crate::modules::sys::get_sys_attr("_xoptions").is_some() {
                V.store(v as u8, std::sync::atomic::Ordering::Relaxed);
            }
            v
        }
        x => x != 0,
    }
}

#[allow(dead_code)]
fn str_tuple(o: &PyObject) -> Option<Vec<String>> {
    if !tuple::is_tuple(o) {
        return None;
    }
    let mut v = Vec::new();
    for it in tuple::tuple_items(o) {
        if !pystr::is_str(it) {
            return None;
        }
        v.push(pystr::to_string_lossy(it));
    }
    Some(v)
}

thread_local! {
    /// Set while `_loads_without_positions` runs.
    static STRIP_COLUMNS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn no_debug_ranges() -> bool {
    if STRIP_COLUMNS.with(|c| c.get()) {
        return true;
    }
    // -X no_debug_ranges / PYTHONNODEBUGRANGES: drop column information on load.
    if std::env::var_os("PYTHONNODEBUGRANGES").map(|v| !v.is_empty()).unwrap_or(false) {
        return true;
    }
    (|| -> PyResult<bool> {
        let sys = crate::import::import_absolute("sys")?;
        let xo = ab::getattr_str(&sys, "_xoptions")?;
        if dict::is_dict(&xo) {
            return Ok(dict::get_item_rs(&xo, "no_debug_ranges").is_some());
        }
        Ok(false)
    })()
    .unwrap_or_else(|_| {
        clear_exc();
        false
    })
}

/// Re-encode a location table without column information (`-X no_debug_ranges`).
fn strip_linetable_columns(lt: &[u8], firstlineno: u32) -> Vec<u8> {
    let entries = crate::interp::wc::decode_linetable_entries(lt, firstlineno);
    let items: Vec<(u32, crate::interp::wc::Position)> = entries
        .iter()
        .map(|e| {
            let mut p = e.pos;
            p.end_lineno = p.lineno;
            p.col = None;
            p.end_col = None;
            (e.len, p)
        })
        .collect();
    crate::interp::wc::encode_linetable(&items, firstlineno)
}

fn build_public_code(p: CodeParts) -> PyResult<PyRef> {
    let invalid = || bad("invalid code object");
    if !bytes::is_bytes(p.code) || !bytes::is_bytes(p.lpkinds) || !bytes::is_bytes(p.linetable) || !bytes::is_bytes(p.exctable) {
        return Err(invalid());
    }
    if !tuple::is_tuple(p.consts) || !pystr::is_str(p.filename) || !pystr::is_str(p.name) || !pystr::is_str(p.qualname) {
        return Err(invalid());
    }
    str_count(p.names).ok_or_else(invalid)?;
    let nlp = str_count(p.lpnames).ok_or_else(invalid)?;
    let kinds = bytes::bytes_data(p.lpkinds).to_vec();
    if kinds.len() != nlp {
        return Err(invalid());
    }
    let intern_all = |t: &PyObject| -> Vec<PyRef> {
        tuple::tuple_items(t)
            .iter()
            .map(|n| if pystr::is_str_exact(n) { pystr::intern(n.to_ref()) } else { n.to_ref() })
            .collect()
    };
    let linetable = if no_debug_ranges_cached() {
        bytes::new_bytes(&strip_linetable_columns(bytes::bytes_data(p.linetable), p.firstlineno.max(0) as u32))
    } else {
        p.linetable.to_ref()
    };
    let consts = if tuple::is_tuple_exact(p.consts) { p.consts.to_ref() } else { ab::sequence_tuple(p.consts)? };
    let r = code_mod::new_code_from_public(code_mod::PublicParts {
        argcount: p.argcount,
        posonlyargcount: p.posonly,
        kwonlyargcount: p.kwonly,
        stacksize: p.stacksize,
        flags: p.flags,
        firstlineno: p.firstlineno,
        code: p.code.to_ref(),
        consts: tuple::tuple_to_vec(&consts),
        names: intern_all(p.names),
        localsplusnames: intern_all(p.lpnames),
        kinds,
        filename: p.filename.to_ref(),
        name: p.name.to_ref(),
        qualname: p.qualname.to_ref(),
        linetable,
        exceptiontable: p.exctable.to_ref(),
    })
    .map_err(|e| {
        if exc_matches(crate::types::exceptions::SystemError()) || exc_matches(crate::types::exceptions::ValueError()) {
            clear_exc();
            invalid()
        } else {
            e
        }
    })?;
    // (core6) keep the co_consts tuple that was read (marshal back-references share it between
    // code objects, as the compiler's constant merging did)
    if tuple::is_tuple_exact(&consts) {
        let c = unsafe { &mut *(r.as_ptr() as *mut crate::types::code::PyCodeObject) };
        if !c.co_consts.is_null() && !consts.is(unsafe { &*c.co_consts }) {
            unsafe { crate::types::code::replace_consts(&r, consts.to_ref()) };
        }
    }
    Ok(r)
}

fn build_code(p: CodeParts) -> PyResult<PyRef> {
    build_public_code(p)
}

/// PyMarshal_ReadObjectFromString equivalent.
pub fn loads_slice(data: &[u8], allow_code: bool) -> PyResult {
    let mut r = Reader { src: Source::Mem { data, pos: 0 }, refs: Vec::new(), depth: 0, allow_code };
    match r.object()? {
        Some(v) => Ok(v),
        None => raise!(TypeError, "NULL object in marshal data for object"),
    }
}

/// Load from bytes; also returns the number of bytes consumed.
/// Load one object from a bytes object.
pub fn loads_bytes(data: &PyObject, allow_code: bool) -> PyResult {
    loads_slice(bytes::bytes_data(data), allow_code)
}

pub fn loads_prefix(data: &[u8], allow_code: bool) -> PyResult<(PyRef, usize)> {
    let mut r = Reader { src: Source::Mem { data, pos: 0 }, refs: Vec::new(), depth: 0, allow_code };
    let v = match r.object()? {
        Some(v) => v,
        None => raise!(TypeError, "NULL object in marshal data for object"),
    };
    let pos = match r.src {
        Source::Mem { pos, .. } => pos,
        _ => 0,
    };
    Ok((v, pos))
}

// ====================================================================== module functions

fn parse_version(o: Option<&PyObject>) -> PyResult<i32> {
    match o {
        None => Ok(VERSION),
        Some(v) => {
            if !int::is_int(v) {
                raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(v));
            }
            let x = ab::as_i64(v)?;
            if x < i32::MIN as i64 || x > i32::MAX as i64 {
                raise!(OverflowError, "Python int too large to convert to C int");
            }
            Ok(x as i32)
        }
    }
}

fn parse_allow_code(o: Option<&PyObject>) -> PyResult<bool> {
    match o {
        None => Ok(true),
        Some(v) => ab::is_true(v),
    }
}

fn m_dumps(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("dumps", &["value", "version", "allow_code"], 2, 1).kwonly(1);
    let [value, version, allow] = crate::args::parse::<3>(&P, args, kw)?;
    let version = parse_version(version)?;
    crate::modules::sys_full::audit("marshal.dumps", &[value.unwrap(), &int::new_int(version as i64)])?;
    let out = dumps_vec(value.unwrap(), version, parse_allow_code(allow)?)?;
    Ok(bytes::new_bytes(&out))
}

fn m_dump(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("dump", &["value", "file", "version", "allow_code"], 3, 2).kwonly(1);
    let [value, file, version, allow] = crate::args::parse::<4>(&P, args, kw)?;
    let version = parse_version(version)?;
    crate::modules::sys_full::audit("marshal.dumps", &[value.unwrap(), &int::new_int(version as i64)])?;
    let out = dumps_vec(value.unwrap(), version, parse_allow_code(allow)?)?;
    ab::call_method_str(file.unwrap(), "write", &[&bytes::new_bytes(&out)])?;
    Ok(none::none_ref())
}

fn m_loads(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("loads", &["bytes", "allow_code"], 1, 1).kwonly(1);
    let [data, allow] = crate::args::parse::<2>(&P, args, kw)?;
    let allow = parse_allow_code(allow)?;
    let data = data.unwrap();
    let data = if bytes::is_bytes(data) {
        data.to_ref()
    } else {
        bytes::new_bytes(&crate::buffer::with_buffer(data, |b| b.to_vec())?)
    };
    crate::modules::sys_full::audit("marshal.loads", &[&data])?;
    loads_slice(bytes::bytes_data(&data), allow)
}

/// Private helper (used by doctest): loads() with column information removed from
/// code objects, as under -X no_debug_ranges.
fn m_loads_without_positions(_m: &PyObject, data: &PyObject) -> PyResult {
    let bytes_data = crate::buffer::with_buffer(data, |b| b.to_vec())?;
    STRIP_COLUMNS.with(|c| c.set(true));
    let r = loads_slice(&bytes_data, true);
    STRIP_COLUMNS.with(|c| c.set(false));
    r
}

fn m_load(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("load", &["file", "allow_code"], 1, 1).kwonly(1);
    let [file, allow] = crate::args::parse::<2>(&P, args, kw)?;
    let allow = parse_allow_code(allow)?;
    let file = file.unwrap();
    crate::modules::sys_full::audit("marshal.load", &[])?;
    // Make sure the object has a read method returning bytes (as CPython does).
    let data = ab::call_method_str(file, "read", &[&int::new_int(0)])?;
    if !bytes::is_bytes(&data) {
        raise!(TypeError, "file.read() returned not bytes but {}", tname(&data));
    }
    let mut r = Reader { src: Source::File { file, consumed: 0 }, refs: Vec::new(), depth: 0, allow_code: allow };
    match r.object()? {
        Some(v) => Ok(v),
        None => raise!(TypeError, "NULL object in marshal data for object"),
    }
}

pub fn init() -> PyResult {
    let methods = mdefs![
        ("dump", FASTKW, m_dump, "dump($module, value, file, version=version, /, *, allow_code=True)\n--\n\nSerialize value with the marshal format and write it to the binary file.\n\nversion selects the format version; allow_code=False rejects code objects.\nRaises ValueError for unsupported types (partial output may have been written)."),
        ("load", FASTKW, m_load, "load($module, file, /, *, allow_code=True)\n--\n\nRead and return one marshalled value from the binary file.\n\nThe file is left positioned just after the value. allow_code=False rejects\ncode objects. Raises EOFError, ValueError or TypeError for bad data."),
        ("dumps", FASTKW, m_dumps, "dumps($module, value, version=version, /, *, allow_code=True)\n--\n\nReturn value serialized with the marshal format as bytes.\n\nversion selects the format version; allow_code=False rejects code objects.\nRaises ValueError for unsupported types."),
        ("_loads_without_positions", O, m_loads_without_positions, "Private: loads() dropping column positions of code objects."),
        ("loads", FASTKW, m_loads, "loads($module, bytes, /, *, allow_code=True)\n--\n\nDeserialize one marshalled value from a bytes-like object.\n\nTrailing bytes are ignored. allow_code=False rejects code objects.\nRaises EOFError, ValueError or TypeError for bad data."),
    ];
    let (m, d) = crate::modules::fncommon::new_module("marshal", methods, Some(MODULE_DOC))?;
    dict::set_item_str(&d, "version", &int::new_int(VERSION as i64))?;
    Ok(m)
}

const MODULE_DOC: &str = "Serialization of Python values in the internal marshal format.\n\nSupported: None, bool, int, float, complex, str, bytes and other\nbytes-like objects, tuple, list, dict, set, frozenset, frozendict, slice,\nEllipsis, StopIteration and code objects (with the containers holding only\nsupported values). The format is not meant for long-term storage or for\nexchange with other implementations.\n\nversion -- the default format version written by dump() and dumps()\n\ndump(), load() -- write a value to, or read a value from, a binary file\ndumps(), loads() -- the same for bytes objects";

// ====================================================================== C API

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMarshal_WriteObjectToString(v: *mut PyObject, version: std::ffi::c_int) -> *mut PyObject {
    unsafe { into_c(dumps_vec(&*v, version, true).map(|b| bytes::new_bytes(&b))) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMarshal_ReadObjectFromString(s: *const std::ffi::c_char, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let data = if n <= 0 { &[][..] } else { std::slice::from_raw_parts(s as *const u8, n as usize) };
        into_c(loads_slice(data, true))
    }
}

fn file_read_all(fp: *mut libc::FILE) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = unsafe { libc::fread(buf.as_mut_ptr() as *mut _, 1, buf.len(), fp) };
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    out
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMarshal_WriteLongToFile(x: std::ffi::c_long, fp: *mut libc::FILE, _version: std::ffi::c_int) {
    let b = (x as i32).to_le_bytes();
    unsafe { libc::fwrite(b.as_ptr() as *const _, 1, 4, fp) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMarshal_WriteObjectToFile(v: *mut PyObject, fp: *mut libc::FILE, version: std::ffi::c_int) {
    unsafe {
        match dumps_vec(&*v, version, true) {
            Ok(b) => {
                libc::fwrite(b.as_ptr() as *const _, 1, b.len(), fp);
            }
            Err(_) => {}
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMarshal_ReadShortFromFile(fp: *mut libc::FILE) -> std::ffi::c_int {
    let mut b = [0u8; 2];
    let n = unsafe { libc::fread(b.as_mut_ptr() as *mut _, 1, 2, fp) };
    if n != 2 {
        let _ = new_err(exceptions::EOFError(), "EOF read where not expected");
        return -1;
    }
    i16::from_le_bytes(b) as std::ffi::c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMarshal_ReadLongFromFile(fp: *mut libc::FILE) -> std::ffi::c_long {
    let mut b = [0u8; 4];
    let n = unsafe { libc::fread(b.as_mut_ptr() as *mut _, 1, 4, fp) };
    if n != 4 {
        let _ = new_err(exceptions::EOFError(), "EOF read where not expected");
        return -1;
    }
    i32::from_le_bytes(b) as std::ffi::c_long
}

fn read_object_from_file(fp: *mut libc::FILE) -> PyResult {
    let start = unsafe { libc::ftell(fp) };
    let data = file_read_all(fp);
    match loads_prefix(&data, true) {
        Ok((v, used)) => {
            if start >= 0 {
                unsafe { libc::fseek(fp, start + used as libc::c_long, libc::SEEK_SET) };
            }
            Ok(v)
        }
        Err(e) => {
            if exc_matches(exceptions::EOFError()) {
                clear_exc();
                raise!(EOFError, "EOF read where not expected");
            }
            Err(e)
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMarshal_ReadObjectFromFile(fp: *mut libc::FILE) -> *mut PyObject {
    into_c(read_object_from_file(fp))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMarshal_ReadLastObjectFromFile(fp: *mut libc::FILE) -> *mut PyObject {
    into_c(read_object_from_file(fp))
}
