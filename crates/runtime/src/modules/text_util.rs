//! Small helpers shared by the text workstream modules (_sre, _codecs, unicodedata,
//! _string, _tokenize, _locale).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::str::{self as pystr, StrData};
use crate::types::{bytes, dict, int, none, tuple};

/// Builder for a str from code points (tracks the maximum character).
#[derive(Default)]
pub struct StrBuf {
    pub v: Vec<u32>,
    pub max: u32,
}

impl StrBuf {
    pub fn new() -> Self {
        StrBuf { v: Vec::new(), max: 0 }
    }
    pub fn with_capacity(n: usize) -> Self {
        StrBuf { v: Vec::with_capacity(n), max: 0 }
    }
    #[inline]
    pub fn push(&mut self, c: u32) {
        if c > self.max {
            self.max = c;
        }
        self.v.push(c);
    }
    pub fn push_str(&mut self, s: &str) {
        for c in s.chars() {
            self.push(c as u32);
        }
    }
    pub fn extend_u32(&mut self, s: &[u32]) {
        for &c in s {
            self.push(c);
        }
    }
    pub fn extend_data(&mut self, d: StrData, a: usize, b: usize) {
        match d {
            StrData::L1(s) => {
                for &c in &s[a..b] {
                    self.push(c as u32);
                }
            }
            StrData::U2(s) => {
                for &c in &s[a..b] {
                    self.push(c as u32);
                }
            }
            StrData::U4(s) => {
                for &c in &s[a..b] {
                    self.push(c);
                }
            }
        }
    }
    pub fn extend_obj(&mut self, o: &PyObject) {
        let d = pystr::str_data(o);
        self.extend_data(d, 0, d.len());
    }
    pub fn len(&self) -> usize {
        self.v.len()
    }
    pub fn is_empty(&self) -> bool {
        self.v.is_empty()
    }
    pub fn finish(&self) -> PyRef {
        pystr::from_codepoints_max(&self.v, self.max)
    }
}

/// `s[a:b]` for a str (exact str result; returns `s` itself for the full exact range).
pub fn substr(s: &PyObject, a: usize, b: usize) -> PyRef {
    let n = pystr::str_len(s);
    let b = b.min(n);
    let a = a.min(b);
    if a == 0 && b == n && pystr::is_str_exact(s) {
        return s.to_ref();
    }
    match pystr::str_data(s) {
        StrData::L1(d) => pystr::from_latin1(&d[a..b]),
        StrData::U2(d) => pystr::from_ucs2(&d[a..b]),
        StrData::U4(d) => pystr::from_codepoints(&d[a..b]),
    }
}

/// Code points of a str object.
pub fn str_cps(o: &PyObject) -> Vec<u32> {
    pystr::str_data(o).to_vec()
}

pub fn new_str_cps(v: &[u32]) -> PyRef {
    pystr::from_codepoints(v)
}

pub fn none() -> PyRef {
    none::none_ref()
}

pub fn new_int(v: i64) -> PyRef {
    int::new_int(v)
}

pub fn tname(o: &PyObject) -> String {
    crate::types::typeobject::tname(o)
}

/// `types.GenericAlias(cls, item)` (used for `__class_getitem__`).
pub fn generic_alias(cls: &PyObject, item: &PyObject) -> PyResult {
    let types = crate::import::import_absolute("types")?;
    let ga = ab::getattr_str(&types, "GenericAlias")?;
    ab::call(&ga, &[cls, item])
}

/// Import `module` and fetch attribute `attr`.
pub fn import_attr(module: &str, attr: &str) -> PyResult {
    let m = crate::import::import_absolute(module)?;
    ab::getattr_str(&m, attr)
}

/// Emit a warning through the `warnings` module (importing it if needed).
pub fn warn(category: *mut PyTypeObject, msg: &str, stacklevel: i32) -> PyResult<()> {
    if crate::import::get_loaded_module("warnings").is_none() {
        let _ = crate::import::import_absolute("warnings");
        clear_exc();
    }
    crate::modules::warnings::warn(category, msg, stacklevel)
}

/// Keep a bytes-like object's contents accessible: bytes are borrowed in place,
/// everything else supporting the buffer protocol is copied.
pub enum ByteData<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
    Held(HeldBuffer),
}

impl ByteData<'_> {
    pub fn as_slice(&self) -> &[u8] {
        match self {
            ByteData::Borrowed(b) => b,
            ByteData::Owned(v) => v,
            ByteData::Held(h) => h.as_slice(),
        }
    }
}

/// A buffer export held until drop (`PyObject_GetBuffer` / `PyBuffer_Release`).
pub struct HeldBuffer {
    view: Box<Py_buffer>,
    obj: PyRef,
}

impl HeldBuffer {
    /// Acquire a simple (contiguous bytes) buffer; `Ok(None)` if `o` has no buffer interface.
    pub fn acquire(o: &PyObject) -> PyResult<Option<HeldBuffer>> {
        let bp = o.ty().tp_as_buffer;
        unsafe {
            if bp.is_null() || (*bp).bf_getbuffer.is_none() {
                return Ok(None);
            }
            let mut view: Box<Py_buffer> = Box::new(std::mem::zeroed());
            if (*bp).bf_getbuffer.unwrap()(o.as_ptr(), &mut *view, 0) < 0 {
                return Err(PyErr);
            }
            Ok(Some(HeldBuffer { view, obj: o.to_ref() }))
        }
    }
    pub fn as_slice(&self) -> &[u8] {
        if self.view.buf.is_null() || self.view.len <= 0 {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(self.view.buf as *const u8, self.view.len as usize) }
    }
}

impl Drop for HeldBuffer {
    fn drop(&mut self) {
        unsafe {
            let bp = self.obj.ty().tp_as_buffer;
            if !bp.is_null() {
                if let Some(r) = (*bp).bf_releasebuffer {
                    r(self.obj.as_ptr(), &mut *self.view);
                }
            }
            if !self.view.obj.is_null() {
                decref(self.view.obj);
            }
        }
    }
}

/// Contents of a bytes-like object. Buffers are held (not copied) while the result lives.
pub fn byte_data(o: &PyObject) -> Option<ByteData<'_>> {
    if bytes::is_bytes(o) {
        return Some(ByteData::Borrowed(bytes::bytes_data(o)));
    }
    match HeldBuffer::acquire(o) {
        Ok(Some(h)) => return Some(ByteData::Held(h)),
        Ok(None) => {}
        Err(_) => clear_exc(),
    }
    if crate::types::bytearray::is_bytearray(o) {
        return Some(ByteData::Owned(crate::types::bytearray::bytearray_data(o).to_vec()));
    }
    None
}

/// Whether `o` supports the buffer protocol (or is bytes/bytearray).
pub fn is_buffer(o: &PyObject) -> bool {
    if bytes::is_bytes(o) || crate::types::bytearray::is_bytearray(o) {
        return true;
    }
    let bp = o.ty().tp_as_buffer;
    !bp.is_null() && unsafe { (*bp).bf_getbuffer.is_some() }
}

/// Build a tuple from owned refs.
pub fn tup(v: Vec<PyRef>) -> PyRef {
    tuple::new_tuple(v)
}

/// Optional str argument as Rust String (None if missing or None).
pub fn opt_str_arg(o: Option<&PyObject>, fname: &str, argname: &str) -> PyResult<Option<String>> {
    match o {
        None => Ok(None),
        Some(v) if none::is_none(v) => Ok(None),
        Some(v) => {
            if !pystr::is_str(v) {
                raise!(TypeError, "{}() argument '{}' must be str or None, not {}", fname, argname, tname(v));
            }
            Ok(Some(pystr::to_string_lossy(v)))
        }
    }
}

/// Set an attribute in a module/dict, ignoring nothing.
pub fn dict_set(d: &PyObject, k: &str, v: &PyObject) -> PyResult<()> {
    dict::set_item_str(d, k, v)
}

/// Finish a static type: fill getattro if unset and call type_ready.
pub fn ready_type(t: *mut PyTypeObject) -> PyResult<()> {
    unsafe {
        if (*t).tp_getattro.is_none() {
            (*t).tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        }
    }
    crate::types::typeobject::type_ready(t)
}

/// Python-style repr of a str (via the runtime's repr).
pub fn repr_string(o: &PyObject) -> PyResult<String> {
    ab::repr_string(o)
}

/// Truncate a string to at most n characters (like `%.<n>R` / `%.<n>U`).
pub fn trunc_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// Register the text workstream's builtin modules.
pub fn register() {
    use crate::import::register_builtin;
    register_builtin("_sre", crate::modules::_sre::init_module);
    register_builtin("_codecs", crate::modules::_codecs::init_module);
    register_builtin("unicodedata", crate::modules::unicodedata::init_module);
}
