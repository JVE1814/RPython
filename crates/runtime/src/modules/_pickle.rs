//! `_pickle`: the native pickler/unpickler (data workstream).
//!
//! Exposes `Pickler`, `Unpickler`, `PickleBuffer`, the exception classes
//! `PickleError`/`PicklingError`/`UnpicklingError`, the memo proxy types and
//! `dump`/`dumps`/`load`/`loads`.  The output format is byte-for-byte the one
//! produced by Lib/pickle.py; error messages and exception notes follow the
//! test suite (spec/Lib/test/pickletester.py).

use crate::abstract_ as ab;
use crate::buffer::{self, BufferView};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{bool_, bytearray, bytes, dict, float, int, list, none, set, str as pystr, tuple};
use crate::{mdefs, name, raise};
use std::collections::HashMap;
use std::ffi::c_int;

// ===================================================================== opcodes

mod op {
    pub const MARK: u8 = b'(';
    pub const STOP: u8 = b'.';
    pub const POP: u8 = b'0';
    pub const POP_MARK: u8 = b'1';
    pub const DUP: u8 = b'2';
    pub const FLOAT: u8 = b'F';
    pub const INT: u8 = b'I';
    pub const BININT: u8 = b'J';
    pub const BININT1: u8 = b'K';
    pub const LONG: u8 = b'L';
    pub const BININT2: u8 = b'M';
    pub const NONE: u8 = b'N';
    pub const PERSID: u8 = b'P';
    pub const BINPERSID: u8 = b'Q';
    pub const REDUCE: u8 = b'R';
    pub const STRING: u8 = b'S';
    pub const BINSTRING: u8 = b'T';
    pub const SHORT_BINSTRING: u8 = b'U';
    pub const UNICODE: u8 = b'V';
    pub const BINUNICODE: u8 = b'X';
    pub const APPEND: u8 = b'a';
    pub const BUILD: u8 = b'b';
    pub const GLOBAL: u8 = b'c';
    pub const DICT: u8 = b'd';
    pub const EMPTY_DICT: u8 = b'}';
    pub const APPENDS: u8 = b'e';
    pub const GET: u8 = b'g';
    pub const BINGET: u8 = b'h';
    pub const INST: u8 = b'i';
    pub const LONG_BINGET: u8 = b'j';
    pub const LIST: u8 = b'l';
    pub const EMPTY_LIST: u8 = b']';
    pub const OBJ: u8 = b'o';
    pub const PUT: u8 = b'p';
    pub const BINPUT: u8 = b'q';
    pub const LONG_BINPUT: u8 = b'r';
    pub const SETITEM: u8 = b's';
    pub const TUPLE: u8 = b't';
    pub const EMPTY_TUPLE: u8 = b')';
    pub const SETITEMS: u8 = b'u';
    pub const BINFLOAT: u8 = b'G';
    pub const PROTO: u8 = 0x80;
    pub const NEWOBJ: u8 = 0x81;
    pub const EXT1: u8 = 0x82;
    pub const EXT2: u8 = 0x83;
    pub const EXT4: u8 = 0x84;
    pub const TUPLE1: u8 = 0x85;
    pub const TUPLE2: u8 = 0x86;
    pub const TUPLE3: u8 = 0x87;
    pub const NEWTRUE: u8 = 0x88;
    pub const NEWFALSE: u8 = 0x89;
    pub const LONG1: u8 = 0x8a;
    pub const LONG4: u8 = 0x8b;
    pub const BINBYTES: u8 = b'B';
    pub const SHORT_BINBYTES: u8 = b'C';
    pub const SHORT_BINUNICODE: u8 = 0x8c;
    pub const BINUNICODE8: u8 = 0x8d;
    pub const BINBYTES8: u8 = 0x8e;
    pub const EMPTY_SET: u8 = 0x8f;
    pub const ADDITEMS: u8 = 0x90;
    pub const FROZENSET: u8 = 0x91;
    pub const NEWOBJ_EX: u8 = 0x92;
    pub const STACK_GLOBAL: u8 = 0x93;
    pub const MEMOIZE: u8 = 0x94;
    pub const FRAME: u8 = 0x95;
    pub const BYTEARRAY8: u8 = 0x96;
    pub const NEXT_BUFFER: u8 = 0x97;
    pub const READONLY_BUFFER: u8 = 0x98;
}

const HIGHEST_PROTOCOL: i64 = 5;
const DEFAULT_PROTOCOL: i64 = 5;
const FRAME_SIZE_MIN: usize = 4;
const FRAME_SIZE_TARGET: usize = 64 * 1024;
const FRAME_HEADER: usize = 9;
const BATCHSIZE: usize = 1000;
const READ_PIECE: usize = 1 << 20;

// ===================================================================== shared state

struct Globals {
    pickle_error: PyRef,
    pickling_error: PyRef,
    unpickling_error: PyRef,
}
static mut GLOBALS: Option<Globals> = None;

fn globals() -> &'static Globals {
    unsafe { (*(&raw const GLOBALS)).as_ref().expect("_pickle not initialised") }
}

fn pickling_error(msg: &str) -> PyErr {
    raise_class(&globals().pickling_error, msg)
}
fn unpickling_error(msg: &str) -> PyErr {
    raise_class(&globals().unpickling_error, msg)
}

fn raise_class(cls: &PyObject, msg: &str) -> PyErr {
    match ab::call(cls, &[&pystr::new_str(msg)]) {
        Ok(e) => {
            set_exc(e);
            PyErr
        }
        Err(e) => e,
    }
}

/// Raise `cls(msg)` with `__context__` set to `ctx` (as an exception raised
/// inside an `except` block would have).
fn raise_with_context(cls: &PyObject, msg: &str, ctx: Option<PyRef>) -> PyErr {
    match ab::call(cls, &[&pystr::new_str(msg)]) {
        Ok(e) => {
            if let Some(c) = ctx {
                let _ = ab::setattr(&e, &pystr::intern_str("__context__"), &c);
                clear_exc();
            }
            set_exc(e);
            PyErr
        }
        Err(e) => e,
    }
}

fn add_note(note: String) -> PyErr {
    if let Some(e) = take_exc() {
        if ab::call_method(&e, name!("add_note"), &[&pystr::new_str(&note)]).is_err() {
            clear_exc();
        }
        set_exc(e);
    }
    PyErr
}

/// Run `f`; if it fails, attach `note()` to the exception.
fn noted<T>(r: PyResult<T>, note: impl FnOnce() -> String) -> PyResult<T> {
    match r {
        Ok(v) => Ok(v),
        Err(_) => Err(add_note(note())),
    }
}

/// Label of the type of `o` used in "when serializing ..." notes.
fn type_label(o: &PyObject) -> String {
    let tp = o.type_ptr();
    let q = type_qualname(unsafe { &*tp });
    match type_module(tp) {
        Some(m) if m != "builtins" && m != "__main__" => format!("{}.{}", m, q),
        _ => q,
    }
}

fn import(name: &str) -> PyResult {
    crate::import::import_absolute(name)
}

fn ts() -> &'static crate::threads::ThreadState {
    crate::threads::current()
}

fn with_recursion<T>(what: &str, f: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    let t = ts();
    crate::interp::enter_recursive_call(t, what)?;
    let r = f();
    crate::interp::leave_recursive_call(t);
    r
}

fn frozendict_type() -> Option<*mut PyTypeObject> {
    match crate::builtins::builtin("frozendict") {
        Ok(fd) if is_type(&fd) => Some(as_type(&fd)),
        Ok(_) => None,
        Err(_) => {
            clear_exc();
            None
        }
    }
}

// ===================================================================== integer helpers

/// Two's complement little-endian bytes of `o`, minimal as pickle requires.
fn encode_long(o: &PyObject) -> PyResult<Vec<u8>> {
    let big = crate::types::int_methods::iref(o);
    if int::sign(o) == 0 {
        return Ok(Vec::new());
    }
    let bits = rpy_bigint::nat::bit_length(int::digits(o)) as usize;
    let n = (bits >> 3) + 1;
    let mut v = rpy_bigint::conv::to_bytes(big, n, true, true).map_err(crate::types::int_methods::int_error)?;
    if int::sign(o) < 0 && v.len() > 1 && v[v.len() - 1] == 0xff && v[v.len() - 2] & 0x80 != 0 {
        v.pop();
    }
    Ok(v)
}

fn decode_long(data: &[u8]) -> PyRef {
    crate::types::int_methods::from_big(rpy_bigint::conv::from_bytes(data, true, true))
}

fn small_int(o: &PyObject) -> Option<i64> {
    if int::is_int(o) { int::as_i64(o) } else { None }
}

// ===================================================================== string helpers

fn utf8_surrogatepass(s: &PyObject) -> Vec<u8> {
    if let Some(a) = pystr::ascii_str(s) {
        return a.as_bytes().to_vec();
    }
    let d = pystr::str_data(s);
    let mut out = Vec::with_capacity(d.len() * 2);
    for c in d.iter() {
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

/// Protocol 0 text form of a str (raw-unicode-escape plus escaping of the
/// characters that would break the line-oriented format).
fn raw_unicode_escape_for_pickle(s: &PyObject) -> Vec<u8> {
    let d = pystr::str_data(s);
    let mut out = Vec::with_capacity(d.len() + 8);
    for c in d.iter() {
        match c {
            0x5c | 0x00 | 0x0a | 0x0d | 0x1a => out.extend_from_slice(format!("\\u{:04x}", c).as_bytes()),
            0..=0xff => out.push(c as u8),
            0x100..=0xffff => out.extend_from_slice(format!("\\u{:04x}", c).as_bytes()),
            _ => out.extend_from_slice(format!("\\U{:08x}", c).as_bytes()),
        }
    }
    out
}

fn decode_utf8_surrogatepass(data: &[u8]) -> PyResult {
    if let Ok(s) = std::str::from_utf8(data) {
        return Ok(pystr::new_str(s));
    }
    let b = bytes::new_bytes(data);
    ab::call_method(&b, name!("decode"), &[&pystr::new_str("utf-8"), &pystr::new_str("surrogatepass")])
}

// ===================================================================== PickleBuffer

#[repr(C)]
struct PickleBufferObj {
    ob_base: PyObject,
    view: *mut Py_buffer, // null once released
    weakreflist: *mut PyObject,
}

static mut PICKLEBUFFER_TYPE: PyTypeObject = PyTypeObject::ZERO;
fn picklebuffer_type() -> *mut PyTypeObject {
    &raw mut PICKLEBUFFER_TYPE
}

fn pb(o: &PyObject) -> &mut PickleBufferObj {
    unsafe { &mut *(o.as_ptr() as *mut PickleBufferObj) }
}

fn pb_view(o: &PyObject) -> PyResult<&mut Py_buffer> {
    let p = pb(o);
    if p.view.is_null() {
        raise!(ValueError, "operation forbidden on released PickleBuffer object");
    }
    Ok(unsafe { &mut *p.view })
}

unsafe extern "C" fn pb_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (pos, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = pos.iter().map(|x| &**x).collect();
            let p = crate::args::Params::new("PickleBuffer", &["buffer"], 1, 1);
            let [b] = crate::args::parse::<1>(&p, &refs, kwn.as_deref())?;
            let mut view = Box::new(buffer::empty_view());
            buffer::get_buffer(b.unwrap(), &mut view, buffer::PyBUF_FULL_RO)?;
            let op = generic_alloc(tp, 0);
            if op.is_null() {
                buffer::release_buffer(&mut *view);
                return Err(PyErr);
            }
            let o = &mut *(op as *mut PickleBufferObj);
            o.view = Box::into_raw(view);
            Ok(PyRef::from_raw(op))
        })();
        into_c(r)
    }
}

fn pb_release_view(o: &PyObject) {
    let p = pb(o);
    if !p.view.is_null() {
        unsafe {
            let mut v = Box::from_raw(p.view);
            p.view = std::ptr::null_mut();
            buffer::release_buffer(&mut *v);
        }
    }
}

unsafe extern "C" fn pb_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        crate::gc::untrack(o);
        if !(*(o as *mut PickleBufferObj)).weakreflist.is_null() {
            crate::types::weakref::clear_weakrefs(o);
        }
        pb_release_view(&*o);
        generic_free_and_type_decref(o, tp);
    }
}

unsafe extern "C" fn pb_traverse(o: *mut PyObject, visit: visitproc, arg: *mut std::ffi::c_void) -> c_int {
    unsafe {
        let p = &*(o as *mut PickleBufferObj);
        if !p.view.is_null() && !(*p.view).obj.is_null() {
            let r = visit.unwrap()((*p.view).obj, arg);
            if r != 0 {
                return r;
            }
        }
        0
    }
}

unsafe extern "C" fn pb_clear(o: *mut PyObject) -> c_int {
    unsafe { pb_release_view(&*o) };
    0
}

unsafe extern "C" fn pb_getbuffer(o: *mut PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let inner = pb_view(&*o)?;
            let owner = &*inner.obj;
            buffer::get_buffer(owner, &mut *view, flags)
        })();
        int_into_c(r)
    }
}

fn pb_raw(o: &PyObject) -> PyResult {
    let v = pb_view(o)?;
    if !buffer::is_contiguous(v, b'A') {
        raise!(BufferError, "cannot extract raw buffer from non-contiguous buffer");
    }
    let m = crate::types::memoryview::from_object(o)?;
    let cdim = ab::getattr_str(&m, "c_contiguous")?;
    if ab::is_true(&cdim)? {
        let ndim = ab::getattr_str(&m, "ndim")?;
        let fmt = ab::getattr_str(&m, "format")?;
        if int::as_i64(&ndim) == Some(1) && pystr::str_eq_ascii(&fmt, "B") {
            return Ok(m);
        }
        return ab::call_method(&m, name!("cast"), &[&pystr::new_str("B")]);
    }
    // Fortran-ordered: expose the underlying memory as flat bytes.
    let flags = if v.readonly != 0 { buffer::PyBUF_READ } else { buffer::PyBUF_WRITE };
    unsafe { crate::types::memoryview::from_memory(v.buf as *mut u8, v.len, flags) }
}

fn pb_release(o: &PyObject) -> PyResult {
    pb_release_view(o);
    Ok(none::none_ref())
}

// ===================================================================== Pickler

struct Memo {
    map: HashMap<usize, (u32, PyRef)>,
}

impl Memo {
    fn new() -> Memo {
        Memo { map: HashMap::new() }
    }
}

struct PState {
    initialized: bool,
    running: bool,
    write: Option<PyRef>,
    proto: i64,
    bin: bool,
    fast: i64,
    fix_imports: bool,
    buffer_callback: Option<PyRef>,
    memo: Memo,
    out: Vec<u8>,
    frame_start: Option<usize>,
    framing: bool,
    // per-dump caches
    pers_func: Option<PyRef>,
    reducer_override: Option<PyRef>,
    dispatch_table: Option<PyRef>,
    fast_nesting: usize,
    fast_memo: HashMap<usize, PyRef>,
}

impl PState {
    fn new() -> PState {
        PState {
            initialized: false,
            running: false,
            write: None,
            proto: DEFAULT_PROTOCOL,
            bin: true,
            fast: 0,
            fix_imports: true,
            buffer_callback: None,
            memo: Memo::new(),
            out: Vec::new(),
            frame_start: None,
            framing: false,
            pers_func: None,
            reducer_override: None,
            dispatch_table: None,
            fast_nesting: 0,
            fast_memo: HashMap::new(),
        }
    }
}

#[repr(C)]
struct PicklerObj {
    ob_base: PyObject,
    dict: *mut PyObject,
    weakreflist: *mut PyObject,
    st: *mut PState,
    /// unused: keeps the instance size of the C implementation (sys.getsizeof)
    _pad: [usize; 14],
}

static mut PICKLER_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut UNPICKLER_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut PMEMO_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut UMEMO_TYPE: PyTypeObject = PyTypeObject::ZERO;
fn pickler_type() -> *mut PyTypeObject {
    &raw mut PICKLER_TYPE
}
fn unpickler_type() -> *mut PyTypeObject {
    &raw mut UNPICKLER_TYPE
}
fn pmemo_type() -> *mut PyTypeObject {
    &raw mut PMEMO_TYPE
}
fn umemo_type() -> *mut PyTypeObject {
    &raw mut UMEMO_TYPE
}

fn pstate(o: &PyObject) -> &mut PState {
    unsafe {
        let p = &mut *(o.as_ptr() as *mut PicklerObj);
        if p.st.is_null() {
            p.st = Box::into_raw(Box::new(PState::new()));
        }
        &mut *p.st
    }
}

unsafe extern "C" fn pickler_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let op = generic_alloc(tp, 0);
        if op.is_null() {
            return op;
        }
        (*(op as *mut PicklerObj)).st = Box::into_raw(Box::new(PState::new()));
        op
    }
}

unsafe extern "C" fn pickler_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        let p = &mut *(o as *mut PicklerObj);
        if !p.weakreflist.is_null() {
            crate::types::weakref::clear_weakrefs(o);
        }
        if !p.st.is_null() {
            drop(Box::from_raw(p.st));
            p.st = std::ptr::null_mut();
        }
        if !p.dict.is_null() {
            crate::object::decref(p.dict);
            p.dict = std::ptr::null_mut();
        }
        generic_free_and_type_decref(o, tp);
    }
}

fn protocol_arg(p: Option<&PyObject>) -> PyResult<i64> {
    let v = match p {
        None => return Ok(DEFAULT_PROTOCOL),
        Some(p) if p.is(none::none()) => return Ok(DEFAULT_PROTOCOL),
        Some(p) => ab::index(p)?,
    };
    let v = match int::as_i64(&v) {
        Some(v) => v,
        None => {
            if int::sign(&v) < 0 {
                -1
            } else {
                i64::MAX
            }
        }
    };
    if v < 0 {
        return Ok(HIGHEST_PROTOCOL);
    }
    if v > HIGHEST_PROTOCOL {
        raise!(ValueError, "pickle protocol must be <= {}", HIGHEST_PROTOCOL);
    }
    Ok(v)
}

fn pickler_setup(o: &PyObject, file: Option<&PyObject>, protocol: Option<&PyObject>, fix_imports: Option<&PyObject>, buffer_callback: Option<&PyObject>) -> PyResult<()> {
    let st = pstate(o);
    if st.running {
        raise!(RuntimeError, "Pickler.__init__() was called while pickling");
    }
    let proto = protocol_arg(protocol)?;
    let fix = match fix_imports {
        Some(f) => ab::is_true(f)?,
        None => true,
    };
    let cb = buffer_callback.filter(|c| !c.is(none::none())).map(|c| c.to_ref());
    if cb.is_some() && proto < 5 {
        raise!(ValueError, "buffer_callback needs protocol >= 5");
    }
    let write = match file {
        Some(f) => match ab::getattr_opt(f, name!("write"))? {
            Some(w) => Some(w),
            None => raise!(TypeError, "file must have a 'write' attribute"),
        },
        None => None,
    };
    st.write = write;
    st.proto = proto;
    st.bin = proto >= 1;
    st.fast = 0;
    st.fix_imports = fix && proto < 3;
    st.buffer_callback = cb;
    st.memo = Memo::new();
    st.out = Vec::with_capacity(4096); // the initial write buffer
    st.initialized = true;
    Ok(())
}

unsafe extern "C" fn pickler_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let (pos, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = pos.iter().map(|x| &**x).collect();
            let p = crate::args::Params::new("Pickler", &["file", "protocol", "fix_imports", "buffer_callback"], 0, 1);
            let [file, proto, fix, cb] = crate::args::parse::<4>(&p, &refs, kwn.as_deref())?;
            pickler_setup(&*o, file, proto, fix, cb)
        })();
        int_into_c(r)
    }
}

struct P<'a> {
    obj: &'a PyObject,
    st: &'a mut PState,
}

static mut DEFAULT_PERSISTENT_ID: *mut PyMethodDef = std::ptr::null_mut();
static mut DEFAULT_PERSISTENT_LOAD: *mut PyMethodDef = std::ptr::null_mut();

/// Is `f` our own default method bound to `owner` (i.e. not overridden)?
fn is_default_method(f: &PyObject, owner: &PyObject, def: *mut PyMethodDef) -> bool {
    if def.is_null() || !crate::types::cfunction::is_cfunction(f) {
        return false;
    }
    let cf = unsafe { &*(f.as_ptr() as *const PyCFunctionObject) };
    cf.m_self == owner.as_ptr() && unsafe { (*cf.m_ml).ml_meth == (*def).ml_meth }
}

impl<'a> P<'a> {
    // ----------------------------------------------------------- output

    #[inline]
    fn w(&mut self, b: &[u8]) {
        self.st.out.extend_from_slice(b);
    }
    #[inline]
    fn w1(&mut self, b: u8) {
        self.st.out.push(b);
    }

    fn start_frame(&mut self) {
        if self.st.framing {
            self.st.frame_start = Some(self.st.out.len());
            self.st.out.extend_from_slice(&[0u8; FRAME_HEADER]);
        }
    }

    /// Close the current frame if it is big enough (or `force`), writing the
    /// FRAME header in place; for file output the bytes are flushed.
    fn commit_frame(&mut self, force: bool) -> PyResult<()> {
        let start = match self.st.frame_start {
            Some(s) => s,
            None => return Ok(()),
        };
        let size = self.st.out.len() - start - FRAME_HEADER;
        if !force && size < FRAME_SIZE_TARGET {
            return Ok(());
        }
        if size >= FRAME_SIZE_MIN {
            self.st.out[start] = op::FRAME;
            self.st.out[start + 1..start + 9].copy_from_slice(&(size as u64).to_le_bytes());
        } else {
            self.st.out.drain(start..start + FRAME_HEADER);
        }
        self.st.frame_start = None;
        if self.st.write.is_some() {
            self.flush()?;
        }
        self.start_frame();
        Ok(())
    }

    fn flush(&mut self) -> PyResult<()> {
        if let Some(wf) = self.st.write.clone() {
            if !self.st.out.is_empty() {
                let b = bytes::new_bytes(&self.st.out);
                self.st.out.clear();
                ab::call(&wf, &[&b])?;
            }
        }
        Ok(())
    }

    /// Header + large payload, outside of any frame.
    fn write_large(&mut self, header: &[u8], payload: &[u8], payload_obj: Option<&PyObject>) -> PyResult<()> {
        if self.st.frame_start.is_some() {
            self.commit_frame(true)?;
            // commit_frame reopened a frame: remove its empty header again
            if let Some(s) = self.st.frame_start.take() {
                self.st.out.truncate(s);
            }
        }
        match self.st.write.clone() {
            Some(wf) => {
                self.w(header);
                self.flush()?;
                let p = match payload_obj {
                    Some(o) if bytes::is_bytes_exact(o) => o.to_ref(),
                    _ => bytes::new_bytes(payload),
                };
                ab::call(&wf, &[&p])?;
            }
            None => {
                self.w(header);
                self.w(payload);
            }
        }
        self.start_frame();
        Ok(())
    }

    // ----------------------------------------------------------- memo

    fn memo_get(&self, o: &PyObject) -> Option<u32> {
        self.st.memo.map.get(&(o.as_ptr() as usize)).map(|e| e.0)
    }

    fn put_code(&mut self, idx: u32) {
        if self.st.proto >= 4 {
            self.w1(op::MEMOIZE);
        } else if !self.st.bin {
            self.w1(op::PUT);
            let s = format!("{}\n", idx);
            self.w(s.as_bytes());
        } else if idx < 256 {
            self.w(&[op::BINPUT, idx as u8]);
        } else {
            self.w1(op::LONG_BINPUT);
            self.w(&idx.to_le_bytes());
        }
    }

    fn get_code(&mut self, idx: u32) {
        if !self.st.bin {
            self.w1(op::GET);
            let s = format!("{}\n", idx);
            self.w(s.as_bytes());
        } else if idx < 256 {
            self.w(&[op::BINGET, idx as u8]);
        } else {
            self.w1(op::LONG_BINGET);
            self.w(&idx.to_le_bytes());
        }
    }

    fn memoize(&mut self, o: &PyObject) {
        if self.st.fast != 0 {
            return;
        }
        let idx = self.st.memo.map.len() as u32;
        self.put_code(idx);
        self.st.memo.map.insert(o.as_ptr() as usize, (idx, o.to_ref()));
    }

    // ----------------------------------------------------------- fast-mode cycle check

    fn fast_enter(&mut self, o: &PyObject) -> PyResult<bool> {
        if self.st.fast == 0 {
            return Ok(false);
        }
        self.st.fast_nesting += 1;
        if self.st.fast_nesting >= 50 {
            let key = o.as_ptr() as usize;
            if self.st.fast_memo.contains_key(&key) {
                self.st.fast_nesting -= 1;
                raise!(ValueError, "fast mode: can't pickle cyclic objects including object of type {} at {:p}", tname(o), o.as_ptr());
            }
            self.st.fast_memo.insert(key, o.to_ref());
            return Ok(true);
        }
        Ok(false)
    }

    fn fast_leave(&mut self, o: &PyObject, registered: bool) {
        if self.st.fast == 0 {
            return;
        }
        if registered {
            self.st.fast_memo.remove(&(o.as_ptr() as usize));
        }
        self.st.fast_nesting = self.st.fast_nesting.saturating_sub(1);
    }

    // ----------------------------------------------------------- save

    fn save(&mut self, o: &PyObject, use_pid: bool) -> PyResult<()> {
        with_recursion(" while pickling an object", || self.save_inner(o, use_pid))
    }

    fn save_inner(&mut self, o: &PyObject, use_pid: bool) -> PyResult<()> {
        self.commit_frame(false)?;
        if use_pid {
            if let Some(f) = self.st.pers_func.clone() {
                let pid = ab::call(&f, &[o])?;
                if !pid.is(none::none()) {
                    return self.save_pers(&pid);
                }
            }
        }
        let tp = o.type_ptr();
        // atomic values are never memoized
        if o.is(none::none()) {
            self.w1(op::NONE);
            return Ok(());
        }
        if tp == bool_::bool_type() {
            return self.save_bool(o);
        }
        if tp == int::int_type() {
            return self.save_int(o);
        }
        if tp == float::float_type() {
            return self.save_float(o);
        }
        if let Some(idx) = self.memo_get(o) {
            self.get_code(idx);
            return Ok(());
        }
        let mut rv: Option<PyRef> = None;
        if let Some(f) = self.st.reducer_override.clone() {
            let r = ab::call(&f, &[o])?;
            if !r.is(none::not_implemented()) {
                rv = Some(r);
            }
        }
        if rv.is_none() {
            if tp == bytes::bytes_type() {
                return self.save_bytes(o);
            }
            if tp == pystr::str_type() {
                return self.save_str(o);
            }
            if tp == tuple::tuple_type() {
                return self.save_tuple(o);
            }
            if tp == list::list_type() {
                return self.save_list(o);
            }
            if tp == dict::dict_type() {
                return self.save_dict(o);
            }
            if tp == set::set_type() {
                return self.save_set(o);
            }
            if tp == set::frozenset_type() {
                return self.save_frozenset(o);
            }
            if tp == bytearray::bytearray_type() {
                return self.save_bytearray(o);
            }
            if tp == picklebuffer_type() {
                return self.save_picklebuffer(o);
            }
            if Some(tp) == frozendict_type() && self.st.proto >= 2 {
                // protocols 0 and 1 cannot express a frozendict: the generic reduce path refuses it
                return self.save_frozendict(o);
            }
            if tp == crate::types::funcobject::function_type() {
                return self.save_global(o, None);
            }
            if tp == type_type() {
                return self.save_type(o);
            }
            // dispatch table
            let table = match self.st.dispatch_table.clone() {
                Some(t) => t,
                None => {
                    let cr = import("copyreg")?;
                    ab::getattr_str(&cr, "dispatch_table")?
                }
            };
            let reducer = if dict::is_dict_exact(&table) {
                dict::get_item(&table, type_obj(tp))?
            } else {
                match ab::getitem(&table, type_obj(tp)) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        if exc_matches(crate::types::exceptions::KeyError()) {
                            clear_exc();
                            None
                        } else {
                            return Err(e);
                        }
                    }
                }
            };
            if let Some(r) = reducer {
                rv = Some(ab::call(&r, &[o])?);
            } else if is_subtype(tp, type_type()) {
                return self.save_global(o, None);
            } else {
                match ab::getattr_opt(o, &pystr::intern_str("__reduce_ex__"))? {
                    Some(m) => rv = Some(ab::call(&m, &[&int::new_int(self.st.proto)])?),
                    None => match ab::getattr_opt(o, &pystr::intern_str("__reduce__"))? {
                        Some(m) => rv = Some(ab::call(&m, &[])?),
                        None => return Err(pickling_error(&format!("Can't pickle {} object", type_label(o)))),
                    },
                }
            }
        }
        let rv = rv.unwrap();
        if pystr::is_str(&rv) {
            return self.save_global(o, Some(&rv));
        }
        let r = (|| -> PyResult<()> {
            if !tuple::is_tuple(&rv) {
                return Err(pickling_error(&format!("__reduce__ must return a string or tuple, not {}", type_label(&rv))));
            }
            let n = tuple::tuple_len(&rv);
            if !(2..=6).contains(&n) {
                return Err(pickling_error("tuple returned by __reduce__ must contain 2 through 6 elements"));
            }
            let items: Vec<PyRef> = tuple::tuple_to_vec(&rv);
            let get = |i: usize| -> Option<&PyObject> { items.get(i).map(|x| &**x).filter(|x| !x.is(none::none())) };
            self.save_reduce(&items[0], &items[1], get(2), get(3), get(4), get(5), Some(o))
        })();
        noted(r, || format!("when serializing {} object", type_label(o)))
    }

    fn save_pers(&mut self, pid: &PyObject) -> PyResult<()> {
        if self.st.bin {
            self.save(pid, false)?;
            self.w1(op::BINPERSID);
            return Ok(());
        }
        let s = ab::str(pid)?;
        match pystr::ascii_str(&s) {
            Some(a) if !a.contains('\n') => {
                let line = format!("{}\n", a);
                self.w1(op::PERSID);
                self.w(line.as_bytes());
                Ok(())
            }
            _ => Err(pickling_error("persistent IDs in protocol 0 must be ASCII strings")),
        }
    }

    fn save_bool(&mut self, o: &PyObject) -> PyResult<()> {
        let t = o.is(bool_::true_obj());
        if self.st.proto >= 2 {
            self.w1(if t { op::NEWTRUE } else { op::NEWFALSE });
        } else {
            self.w(if t { b"I01\n" } else { b"I00\n" });
        }
        Ok(())
    }

    fn save_int(&mut self, o: &PyObject) -> PyResult<()> {
        if let Some(v) = int::as_i64(o) {
            if self.st.bin {
                if (0..0x100).contains(&v) {
                    self.w(&[op::BININT1, v as u8]);
                    return Ok(());
                }
                if (0..0x10000).contains(&v) {
                    self.w1(op::BININT2);
                    self.w(&(v as u16).to_le_bytes());
                    return Ok(());
                }
                if (-0x8000_0000..0x8000_0000).contains(&v) {
                    self.w1(op::BININT);
                    self.w(&(v as i32).to_le_bytes());
                    return Ok(());
                }
            } else if self.st.proto < 2 && (-0x8000_0000..0x8000_0000).contains(&v) {
                let s = format!("I{}\n", v);
                self.w(s.as_bytes());
                return Ok(());
            }
        }
        if self.st.proto >= 2 {
            let data = encode_long(o)?;
            if data.len() < 256 {
                self.w(&[op::LONG1, data.len() as u8]);
            } else {
                if data.len() > i32::MAX as usize {
                    raise!(OverflowError, "int too large to pickle");
                }
                self.w1(op::LONG4);
                self.w(&(data.len() as i32).to_le_bytes());
            }
            self.w(&data);
            return Ok(());
        }
        let r = ab::repr(&crate::types::int_methods::exact_copy(o))?;
        let s = pystr::to_string_lossy(&r);
        let in_range = matches!(int::as_i64(o), Some(v) if (-0x8000_0000..0x8000_0000).contains(&v));
        if in_range {
            let line = format!("I{}\n", s);
            self.w(line.as_bytes());
        } else {
            let line = format!("L{}L\n", s);
            self.w(line.as_bytes());
        }
        Ok(())
    }

    fn save_float(&mut self, o: &PyObject) -> PyResult<()> {
        let v = float::float_value(o);
        if self.st.bin {
            self.w1(op::BINFLOAT);
            self.w(&v.to_be_bytes());
        } else {
            let r = ab::repr(&float::new_float(v))?;
            let line = format!("F{}\n", pystr::to_string_lossy(&r));
            self.w(line.as_bytes());
        }
        Ok(())
    }

    fn write_sized(&mut self, data: &[u8], obj: Option<&PyObject>, short_op: Option<u8>, op4: u8, op8: Option<u8>) -> PyResult<()> {
        let n = data.len();
        if let (Some(s), true) = (short_op, n <= 0xff) {
            self.w(&[s, n as u8]);
            self.w(data);
        } else if let (Some(o8), true) = (op8, n > 0xffff_ffff) {
            let mut h = vec![o8];
            h.extend_from_slice(&(n as u64).to_le_bytes());
            self.write_large(&h, data, obj)?;
        } else {
            if n > 0xffff_ffff {
                raise!(OverflowError, "serializing a bytes object larger than 4 GiB requires pickle protocol 4 or higher");
            }
            let mut h = vec![op4];
            h.extend_from_slice(&(n as u32).to_le_bytes());
            if n >= FRAME_SIZE_TARGET {
                self.write_large(&h, data, obj)?;
            } else {
                self.w(&h);
                self.w(data);
            }
        }
        Ok(())
    }

    fn save_bytes_data(&mut self, data: &[u8], obj: Option<&PyObject>) -> PyResult<()> {
        let op8 = if self.st.proto >= 4 { Some(op::BINBYTES8) } else { None };
        self.write_sized(data, obj, Some(op::SHORT_BINBYTES), op::BINBYTES, op8)
    }

    fn save_bytes(&mut self, o: &PyObject) -> PyResult<()> {
        if self.st.proto < 3 {
            let data = bytes::bytes_data(o);
            if data.is_empty() {
                return self.save_reduce(type_obj(bytes::bytes_type()), &tuple::empty_tuple(), None, None, None, None, Some(o));
            }
            let codecs = import("codecs")?;
            let enc = ab::getattr_str(&codecs, "encode")?;
            let text = pystr::from_latin1(data);
            let args = tuple::tuple2(text, pystr::new_str("latin1"));
            return self.save_reduce(&enc, &args, None, None, None, None, Some(o));
        }
        let data = bytes::bytes_data(o).to_vec();
        self.save_bytes_data(&data, Some(o))?;
        self.memoize(o);
        Ok(())
    }

    fn save_bytearray_data(&mut self, data: &[u8]) -> PyResult<()> {
        let mut h = vec![op::BYTEARRAY8];
        h.extend_from_slice(&(data.len() as u64).to_le_bytes());
        if data.len() >= FRAME_SIZE_TARGET {
            self.write_large(&h, data, None)
        } else {
            self.w(&h);
            self.w(data);
            Ok(())
        }
    }

    fn save_bytearray(&mut self, o: &PyObject) -> PyResult<()> {
        if self.st.proto < 5 {
            let data = bytearray::bytearray_data(o);
            let args = if data.is_empty() { tuple::empty_tuple() } else { tuple::new_tuple(vec![bytes::new_bytes(data)]) };
            return self.save_reduce(type_obj(bytearray::bytearray_type()), &args, None, None, None, None, Some(o));
        }
        let data = bytearray::bytearray_data(o).to_vec();
        self.save_bytearray_data(&data)?;
        self.memoize(o);
        Ok(())
    }

    fn save_picklebuffer(&mut self, o: &PyObject) -> PyResult<()> {
        if self.st.proto < 5 {
            return Err(pickling_error("PickleBuffer can only be pickled with protocol >= 5"));
        }
        let view = BufferView::get(o, buffer::PyBUF_FULL_RO)?;
        if !buffer::is_contiguous(&view.view, b'A') {
            return Err(pickling_error("PickleBuffer can not be pickled when pointing to a non-contiguous buffer"));
        }
        let readonly = view.view.readonly != 0;
        let in_band = match self.st.buffer_callback.clone() {
            None => true,
            Some(cb) => {
                let r = ab::call(&cb, &[o])?;
                ab::is_true(&r)?
            }
        };
        if in_band {
            let data = unsafe { std::slice::from_raw_parts(view.view.buf as *const u8, view.view.len as usize) }.to_vec();
            if readonly {
                self.save_bytes_data(&data, None)?;
            } else {
                self.save_bytearray_data(&data)?;
            }
        } else {
            self.w1(op::NEXT_BUFFER);
            if readonly {
                self.w1(op::READONLY_BUFFER);
            }
        }
        drop(view);
        self.memoize(o);
        Ok(())
    }

    fn save_str(&mut self, o: &PyObject) -> PyResult<()> {
        if !self.st.bin {
            let data = raw_unicode_escape_for_pickle(o);
            self.w1(op::UNICODE);
            self.w(&data);
            self.w1(b'\n');
        } else {
            let data = utf8_surrogatepass(o);
            let big = self.st.proto >= 4;
            self.write_sized(&data, None, if big { Some(op::SHORT_BINUNICODE) } else { None }, op::BINUNICODE, if big { Some(op::BINUNICODE8) } else { None })?;
        }
        self.memoize(o);
        Ok(())
    }

    fn save_tuple(&mut self, o: &PyObject) -> PyResult<()> {
        let n = tuple::tuple_len(o);
        if n == 0 {
            if self.st.bin {
                self.w1(op::EMPTY_TUPLE);
            } else {
                self.w(&[op::MARK, op::TUPLE]);
            }
            return Ok(());
        }
        let short = n <= 3 && self.st.proto >= 2;
        let reg = self.fast_enter(o)?;
        if !short {
            self.w1(op::MARK);
        }
        for i in 0..n {
            let item = tuple::tuple_get(o, i).to_ref();
            let r = self.save(&item, true);
            if r.is_err() {
                self.fast_leave(o, reg);
                return Err(add_note(format!("when serializing {} item {}", type_label(o), i)));
            }
        }
        self.fast_leave(o, reg);
        if let Some(idx) = self.memo_get(o) {
            if short {
                for _ in 0..n {
                    self.w1(op::POP);
                }
            } else if self.st.bin {
                self.w1(op::POP_MARK);
            } else {
                for _ in 0..=n {
                    self.w1(op::POP);
                }
            }
            self.get_code(idx);
            return Ok(());
        }
        if short {
            self.w1([op::EMPTY_TUPLE, op::TUPLE1, op::TUPLE2, op::TUPLE3][n]);
        } else {
            self.w1(op::TUPLE);
        }
        self.memoize(o);
        Ok(())
    }

    fn save_list(&mut self, o: &PyObject) -> PyResult<()> {
        if self.st.bin {
            self.w1(op::EMPTY_LIST);
        } else {
            self.w(&[op::MARK, op::LIST]);
        }
        self.memoize(o);
        let reg = self.fast_enter(o)?;
        let r = self.list_appends(o);
        self.fast_leave(o, reg);
        r
    }

    /// Items of an exact list, appended in batches; the list may shrink
    /// while it is being saved.
    fn list_appends(&mut self, l: &PyObject) -> PyResult<()> {
        if !self.st.bin {
            let mut i = 0;
            while let Some(item) = list::get(l, i) {
                noted(self.save(&item, true), || format!("when serializing {} item {}", type_label(l), i))?;
                self.w1(op::APPEND);
                i += 1;
            }
            return Ok(());
        }
        let mut i = 0;
        loop {
            let total = list::list_len(l);
            if i >= total {
                return Ok(());
            }
            let chunk = (total - i).min(BATCHSIZE);
            if chunk == 1 {
                let item = list::get(l, i).unwrap();
                noted(self.save(&item, true), || format!("when serializing {} item {}", type_label(l), i))?;
                self.w1(op::APPEND);
                i += 1;
            } else {
                self.w1(op::MARK);
                let end = i + chunk;
                while i < end {
                    let item = match list::get(l, i) {
                        Some(v) => v,
                        None => break,
                    };
                    noted(self.save(&item, true), || format!("when serializing {} item {}", type_label(l), i))?;
                    i += 1;
                }
                self.w1(op::APPENDS);
            }
            if chunk < BATCHSIZE {
                return Ok(());
            }
        }
    }

    /// Generic APPEND(S) from an iterator (4th reduce item).
    fn batch_appends(&mut self, it: &PyObject, owner: Option<&PyObject>) -> PyResult<()> {
        let label = owner.map(type_label).unwrap_or_else(|| "NoneType".into());
        let mut index = 0usize;
        if !self.st.bin {
            while let Some(item) = ab::iter_next(it)? {
                noted(self.save(&item, true), || format!("when serializing {} item {}", label, index))?;
                self.w1(op::APPEND);
                index += 1;
            }
            return Ok(());
        }
        loop {
            let mut chunk = Vec::new();
            while chunk.len() < BATCHSIZE {
                match ab::iter_next(it)? {
                    Some(v) => chunk.push(v),
                    None => break,
                }
            }
            let n = chunk.len();
            if n > 1 {
                self.w1(op::MARK);
            }
            for item in chunk.iter() {
                noted(self.save(item, true), || format!("when serializing {} item {}", label, index))?;
                index += 1;
            }
            if n > 1 {
                self.w1(op::APPENDS);
            } else if n == 1 {
                self.w1(op::APPEND);
            }
            if n < BATCHSIZE {
                return Ok(());
            }
        }
    }

    fn save_pair(&mut self, k: &PyObject, v: &PyObject, label: &str) -> PyResult<()> {
        self.save(k, true)?;
        if self.save(v, true).is_err() {
            let kr = ab::repr_string(k).unwrap_or_else(|_| {
                clear_exc();
                "?".into()
            });
            return Err(add_note(format!("when serializing {} item {}", label, kr)));
        }
        Ok(())
    }

    /// SETITEM(S) from an iterator of 2-tuples.
    fn batch_setitems(&mut self, it: &PyObject, owner: Option<&PyObject>) -> PyResult<()> {
        let label = owner.map(type_label).unwrap_or_else(|| "NoneType".into());
        let next_pair = |it: &PyObject| -> PyResult<Option<(PyRef, PyRef)>> {
            match ab::iter_next(it)? {
                None => Ok(None),
                Some(p) => {
                    if !tuple::is_tuple(&p) || tuple::tuple_len(&p) != 2 {
                        raise!(TypeError, "dict items iterator must return 2-tuples");
                    }
                    Ok(Some((tuple::tuple_get(&p, 0).to_ref(), tuple::tuple_get(&p, 1).to_ref())))
                }
            }
        };
        if !self.st.bin {
            while let Some((k, v)) = next_pair(it)? {
                self.save_pair(&k, &v, &label)?;
                self.w1(op::SETITEM);
            }
            return Ok(());
        }
        loop {
            let mut chunk = Vec::new();
            while chunk.len() < BATCHSIZE {
                match next_pair(it)? {
                    Some(p) => chunk.push(p),
                    None => break,
                }
            }
            let n = chunk.len();
            if n > 1 {
                self.w1(op::MARK);
            }
            for (k, v) in chunk.iter() {
                self.save_pair(k, v, &label)?;
            }
            if n > 1 {
                self.w1(op::SETITEMS);
            } else if n == 1 {
                self.w1(op::SETITEM);
            }
            if n < BATCHSIZE {
                return Ok(());
            }
        }
    }

    fn save_dict(&mut self, o: &PyObject) -> PyResult<()> {
        if self.st.bin {
            self.w1(op::EMPTY_DICT);
        } else {
            self.w(&[op::MARK, op::DICT]);
        }
        self.memoize(o);
        let reg = self.fast_enter(o)?;
        let r = (|| -> PyResult<()> {
            let items = ab::call_method(o, name!("items"), &[])?;
            let it = ab::get_iter(&items)?;
            self.batch_setitems(&it, Some(o))
        })();
        self.fast_leave(o, reg);
        r
    }

    fn save_set(&mut self, o: &PyObject) -> PyResult<()> {
        if self.st.proto < 4 {
            let l = list::list_from_iter(o)?;
            return self.save_reduce(type_obj(set::set_type()), &tuple::new_tuple(vec![l]), None, None, None, None, Some(o));
        }
        self.w1(op::EMPTY_SET);
        self.memoize(o);
        let it = ab::get_iter(o)?;
        loop {
            let mut chunk = Vec::new();
            while chunk.len() < BATCHSIZE {
                match ab::iter_next(&it)? {
                    Some(v) => chunk.push(v),
                    None => break,
                }
            }
            if !chunk.is_empty() {
                self.w1(op::MARK);
                for item in chunk.iter() {
                    noted(self.save(item, true), || format!("when serializing {} element", type_label(o)))?;
                }
                self.w1(op::ADDITEMS);
            }
            if chunk.len() < BATCHSIZE {
                return Ok(());
            }
        }
    }

    fn save_frozenset(&mut self, o: &PyObject) -> PyResult<()> {
        if self.st.proto < 4 {
            let l = list::list_from_iter(o)?;
            return self.save_reduce(type_obj(set::frozenset_type()), &tuple::new_tuple(vec![l]), None, None, None, None, Some(o));
        }
        let reg = self.fast_enter(o)?;
        self.w1(op::MARK);
        let r = (|| -> PyResult<()> {
            let it = ab::get_iter(o)?;
            while let Some(item) = ab::iter_next(&it)? {
                noted(self.save(&item, true), || format!("when serializing {} element", type_label(o)))?;
            }
            Ok(())
        })();
        self.fast_leave(o, reg);
        r?;
        if let Some(idx) = self.memo_get(o) {
            self.w1(op::POP_MARK);
            self.get_code(idx);
            return Ok(());
        }
        self.w1(op::FROZENSET);
        self.memoize(o);
        Ok(())
    }

    fn save_frozendict(&mut self, o: &PyObject) -> PyResult<()> {
        let cls = type_obj(o.type_ptr());
        if self.st.proto < 2 {
            let d = ab::call(type_obj(dict::dict_type()), &[o])?;
            return self.save_reduce(cls, &tuple::new_tuple(vec![d]), None, None, None, None, Some(o));
        }
        noted(self.save(cls, true), || format!("when serializing {} reconstructor", type_label(o)))?;
        self.w1(op::EMPTY_DICT);
        let reg = self.fast_enter(o)?;
        let r = (|| -> PyResult<()> {
            let items = ab::call_method(o, name!("items"), &[])?;
            let it = ab::get_iter(&items)?;
            self.batch_setitems(&it, Some(o))
        })();
        self.fast_leave(o, reg);
        r?;
        if let Some(idx) = self.memo_get(o) {
            self.w(&[op::POP, op::POP]);
            self.get_code(idx);
            return Ok(());
        }
        self.w(&[op::TUPLE1, op::REDUCE]);
        self.memoize(o);
        Ok(())
    }

    fn save_type(&mut self, o: &PyObject) -> PyResult<()> {
        let singletons = [none::none(), none::not_implemented(), none::ellipsis()];
        for s in singletons {
            if o.is(type_obj(s.type_ptr())) {
                let args = tuple::new_tuple(vec![s.to_ref()]);
                return self.save_reduce(type_obj(type_type()), &args, None, None, None, None, Some(o));
            }
        }
        self.save_global(o, None)
    }

    // ----------------------------------------------------------- reduce

    #[allow(clippy::too_many_arguments)]
    fn save_reduce(&mut self, func: &PyObject, args: &PyObject, state: Option<&PyObject>, listitems: Option<&PyObject>, dictitems: Option<&PyObject>, state_setter: Option<&PyObject>, obj: Option<&PyObject>) -> PyResult<()> {
        if !ab::is_callable(func) {
            return Err(pickling_error(&format!("first item of the tuple returned by __reduce__ must be callable, not {}", type_label(func))));
        }
        if !tuple::is_tuple(args) {
            return Err(pickling_error(&format!("second item of the tuple returned by __reduce__ must be a tuple, not {}", type_label(args))));
        }
        if let Some(l) = listitems {
            if !is_iterator(l) {
                return Err(pickling_error(&format!("fourth item of the tuple returned by __reduce__ must be an iterator, not {}", type_label(l))));
            }
        }
        if let Some(d) = dictitems {
            if !is_iterator(d) {
                return Err(pickling_error(&format!("fifth item of the tuple returned by __reduce__ must be an iterator, not {}", type_label(d))));
            }
        }
        if let Some(s) = state_setter {
            if !ab::is_callable(s) {
                return Err(pickling_error(&format!("sixth item of the tuple returned by __reduce__ must be callable, not {}", type_label(s))));
            }
        }
        let label = obj.map(type_label).unwrap_or_else(|| "NoneType".into());
        let fname = if self.st.proto >= 2 {
            match ab::getattr_opt(func, &pystr::intern_str("__name__"))? {
                Some(n) if pystr::is_str(&n) => pystr::to_string_lossy(&n),
                _ => String::new(),
            }
        } else {
            String::new()
        };
        if fname == "__newobj_ex__" {
            let n = tuple::tuple_len(args);
            if n != 3 {
                return Err(pickling_error(&format!("__newobj_ex__ expected 3 arguments, got {}", n)));
            }
            let cls = tuple::tuple_get(args, 0).to_ref();
            let cargs = tuple::tuple_get(args, 1).to_ref();
            let kwargs = tuple::tuple_get(args, 2).to_ref();
            if !is_type(&cls) {
                return Err(pickling_error(&format!("first argument to __newobj_ex__() must be a class, not {}", type_label(&cls))));
            }
            if !tuple::is_tuple(&cargs) {
                return Err(pickling_error(&format!("second argument to __newobj_ex__() must be a tuple, not {}", type_label(&cargs))));
            }
            if !dict::is_dict(&kwargs) {
                return Err(pickling_error(&format!("third argument to __newobj_ex__() must be a dict, not {}", type_label(&kwargs))));
            }
            if self.st.proto >= 4 {
                noted(self.save(&cls, true), || format!("when serializing {} class", label))?;
                noted(self.save(&cargs, true).and_then(|_| self.save(&kwargs, true)), || format!("when serializing {} __new__ arguments", label))?;
                self.w1(op::NEWOBJ_EX);
            } else {
                let functools = import("functools")?;
                let partial = ab::getattr_str(&functools, "partial")?;
                let new = ab::getattr(&cls, name!("__new__"))?;
                let mut pargs: Vec<&PyObject> = vec![&new, &cls];
                let citems: Vec<PyRef> = tuple::tuple_to_vec(&cargs);
                for c in citems.iter() {
                    pargs.push(c);
                }
                let builder = ab::call_with_kwargs_dict(&partial, &pargs, Some(&kwargs))?;
                noted(self.save(&builder, true), || format!("when serializing {} reconstructor", label))?;
                self.save(&tuple::empty_tuple(), true)?;
                self.w1(op::REDUCE);
            }
        } else if fname == "__newobj__" {
            let n = tuple::tuple_len(args);
            if n < 1 {
                return Err(pickling_error("__newobj__ expected at least 1 argument, got 0"));
            }
            let cls = tuple::tuple_get(args, 0).to_ref();
            if !is_type(&cls) {
                return Err(pickling_error(&format!("first argument to __newobj__() must be a class, not {}", type_label(&cls))));
            }
            if let Some(o) = obj {
                let oc = ab::getattr(o, name!("__class__"))?;
                if !oc.is(&cls) {
                    let a = ab::repr_string(&oc)?;
                    let b = ab::repr_string(&cls)?;
                    return Err(pickling_error(&format!("first argument to __newobj__() must be {}, not {}", a, b)));
                }
            }
            let rest = tuple::new_tuple(tuple::tuple_to_vec(args)[1..].to_vec());
            noted(self.save(&cls, true), || format!("when serializing {} class", label))?;
            noted(self.save(&rest, true), || format!("when serializing {} __new__ arguments", label))?;
            self.w1(op::NEWOBJ);
        } else {
            noted(self.save(func, true), || format!("when serializing {} reconstructor", label))?;
            noted(self.save(args, true), || format!("when serializing {} reconstructor arguments", label))?;
            self.w1(op::REDUCE);
        }
        if let Some(o) = obj {
            if let Some(idx) = self.memo_get(o) {
                self.w1(op::POP);
                self.get_code(idx);
            } else {
                self.memoize(o);
            }
        }
        if let Some(l) = listitems {
            self.batch_appends(l, obj)?;
        }
        if let Some(d) = dictitems {
            self.batch_setitems(d, obj)?;
        }
        if let Some(s) = state {
            match state_setter {
                None => {
                    noted(self.save(s, true), || format!("when serializing {} state", label))?;
                    self.w1(op::BUILD);
                }
                Some(setter) => {
                    noted(self.save(setter, true), || format!("when serializing {} state setter", label))?;
                    if let Some(o) = obj {
                        self.save(o, true)?;
                    } else {
                        self.w1(op::NONE);
                    }
                    noted(self.save(s, true), || format!("when serializing {} state", label))?;
                    self.w(&[op::TUPLE2, op::REDUCE, op::POP]);
                }
            }
        }
        Ok(())
    }

    // ----------------------------------------------------------- globals

    fn save_global(&mut self, o: &PyObject, given: Option<&PyObject>) -> PyResult<()> {
        let name: PyRef = match given {
            Some(n) => n.to_ref(),
            None => match ab::getattr_opt(o, &pystr::intern_str("__qualname__"))? {
                Some(q) => q,
                None => ab::getattr(o, name!("__name__"))?,
            },
        };
        // Names are handled as Rust strings below, which cannot hold lone
        // surrogates: such a name can never be written (protocol 3+ uses
        // UTF-8), so report it now (grind-lib).
        if self.st.proto >= 3 {
            let module_attr = ab::getattr_opt(o, &pystr::intern_str("__module__"))?;
            for (what, s) in [("module", module_attr.filter(|m| pystr::is_str(m))), ("global", Some(name.clone()))] {
                if let Some(s) = s {
                    if pystr::as_utf8(&s).is_err() {
                        let ctx = take_exc();
                        let r = ab::repr_string(&s)?;
                        return Err(raise_with_context(
                            &globals().pickling_error,
                            &format!("can't pickle {} identifier {} using pickle protocol {}", what, r, self.st.proto),
                            ctx,
                        ));
                    }
                }
            }
        }
        let name_s = pystr::to_string_lossy(&name);
        let module_name = whichmodule(o, &name, &name_s)?;
        let module_s = pystr::to_string_lossy(&module_name);
        if self.st.proto >= 2 {
            let cr = import("copyreg")?;
            let reg = ab::getattr_str(&cr, "_extension_registry")?;
            let key = tuple::tuple2(module_name.clone(), name.clone());
            let code = if dict::is_dict(&reg) { dict::get_item(&reg, &key)? } else { None };
            if let Some(code) = code {
                return self.write_ext(&code);
            }
        }
        let mut path: Vec<String> = name_s.split('.').map(|s| s.to_string()).collect();
        if path.iter().any(|p| p.starts_with("__") && !p.ends_with("__")) {
            let modules = crate::modules::sys::modules_dict();
            if let Some(m) = dict::get_item(&modules, &module_name)? {
                path = concrete_path(&m, &path)?;
            }
        }
        let full = path.join(".");
        if self.st.proto >= 4 {
            self.save(&module_name, true)?;
            self.save(&pystr::new_str(&full), true)?;
            self.w1(op::STACK_GLOBAL);
        } else if path.len() > 1 {
            let getattr = crate::builtins::builtin("getattr")?;
            for _ in 1..path.len() {
                self.save(&getattr, true)?;
                if self.st.proto < 2 {
                    self.w1(op::MARK);
                }
            }
            self.save_toplevel(&module_s, &path[0])?;
            for attr in path[1..].iter() {
                self.save(&pystr::new_str(attr), true)?;
                self.w1(if self.st.proto < 2 { op::TUPLE } else { op::TUPLE2 });
                self.w1(op::REDUCE);
            }
        } else {
            self.save_toplevel(&module_s, &full)?;
        }
        self.memoize(o);
        Ok(())
    }

    fn write_ext(&mut self, code: &PyObject) -> PyResult<()> {
        let le = |a: &PyObject, b: i64| ab::rich_compare_bool(a, &int::new_int(b), Py_LE);
        if le(code, 0xff)? {
            let v = small_int(code).unwrap_or(-1);
            if !(0..=0xff).contains(&v) {
                return Err(crate::errors::new_err(struct_error(), "ubyte format requires 0 <= number <= 255"));
            }
            if v == 0 {
                raise!(RuntimeError, "extension code 0 is out of range");
            }
            self.w(&[op::EXT1, v as u8]);
        } else if le(code, 0xffff)? {
            let v = small_int(code).unwrap();
            self.w1(op::EXT2);
            self.w(&(v as u16).to_le_bytes());
        } else {
            let v = match small_int(code) {
                Some(v) if v <= i32::MAX as i64 => v,
                _ => return Err(crate::errors::new_err(struct_error(), "'i' format requires -2147483648 <= number <= 2147483647")),
            };
            self.w1(op::EXT4);
            self.w(&(v as i32).to_le_bytes());
        }
        Ok(())
    }

    fn save_toplevel(&mut self, module: &str, name: &str) -> PyResult<()> {
        let (mut module, mut name) = (module.to_string(), name.to_string());
        let ascii = self.st.proto < 3;
        if ascii && self.st.fix_imports {
            let cp = import("_compat_pickle")?;
            let rn = ab::getattr_str(&cp, "REVERSE_NAME_MAPPING")?;
            let ri = ab::getattr_str(&cp, "REVERSE_IMPORT_MAPPING")?;
            let key = tuple::tuple2(pystr::new_str(&module), pystr::new_str(&name));
            if let Some(v) = dict::get_item(&rn, &key)? {
                if tuple::is_tuple(&v) && tuple::tuple_len(&v) == 2 {
                    module = pystr::to_string_lossy(tuple::tuple_get(&v, 0));
                    name = pystr::to_string_lossy(tuple::tuple_get(&v, 1));
                }
            } else if let Some(v) = dict::get_item(&ri, &pystr::new_str(&module))? {
                module = pystr::to_string_lossy(&v);
            }
        }
        let encode = |s: &str, what: &str| -> PyResult<Vec<u8>> {
            let so = pystr::new_str(s);
            let enc = if ascii { "ascii" } else { "utf-8" };
            match ab::call_method(&so, name!("encode"), &[&pystr::new_str(enc)]) {
                Ok(b) => Ok(bytes::bytes_data(&b).to_vec()),
                Err(_) => {
                    let ctx = take_exc();
                    let r = ab::repr_string(&so).unwrap_or_default();
                    Err(raise_with_context(&globals().pickling_error, &format!("can't pickle {} identifier {} using pickle protocol {}", what, r, self.st.proto), ctx))
                }
            }
        };
        // keep the original object's text for error messages (surrogates)
        let mb = encode(&module, "module")?;
        let nb = encode(&name, "global")?;
        self.w1(op::GLOBAL);
        self.w(&mb);
        self.w1(b'\n');
        self.w(&nb);
        self.w1(b'\n');
        Ok(())
    }

    fn dump(&mut self, o: &PyObject) -> PyResult<()> {
        // per-dump lookups of the overridable hooks
        let pid = ab::getattr_opt(self.obj, &pystr::intern_str("persistent_id"))?;
        self.st.pers_func = pid.filter(|f| !is_default_method(f, self.obj, unsafe { DEFAULT_PERSISTENT_ID }));
        self.st.reducer_override = ab::getattr_opt(self.obj, &pystr::intern_str("reducer_override"))?;
        self.st.dispatch_table = ab::getattr_opt(self.obj, &pystr::intern_str("dispatch_table"))?;
        self.st.fast_nesting = 0;
        self.st.fast_memo.clear();
        self.st.out.clear();
        if self.st.proto >= 2 {
            self.w(&[op::PROTO, self.st.proto as u8]);
        }
        self.st.framing = self.st.proto >= 4;
        self.start_frame();
        let r = self.save(o, true);
        if r.is_ok() {
            self.w1(op::STOP);
        }
        let r2 = if r.is_ok() { self.commit_frame(true) } else { Ok(()) };
        if let Some(s) = self.st.frame_start.take() {
            // drop the empty frame header opened after the last commit
            if self.st.out.len() == s + FRAME_HEADER {
                self.st.out.truncate(s);
            }
        }
        self.st.framing = false;
        self.st.pers_func = None;
        self.st.reducer_override = None;
        self.st.dispatch_table = None;
        self.st.fast_memo.clear();
        r?;
        r2?;
        if self.st.write.is_some() {
            self.flush()?;
            // the write buffer is released after every dump()
            self.st.out = Vec::new();
        }
        Ok(())
    }
}

fn struct_error() -> *mut PyTypeObject {
    match import("struct").and_then(|m| ab::getattr_str(&m, "error")) {
        Ok(e) if is_type(&e) => as_type(&e),
        _ => {
            clear_exc();
            crate::types::exceptions::ValueError()
        }
    }
}

fn mangle(owner: &PyObject, name: &str) -> Option<String> {
    if is_type(owner) && name.starts_with("__") && !name.ends_with("__") {
        let base = type_name(unsafe { &*as_type(owner) });
        let base = base.trim_start_matches('_');
        if !base.is_empty() {
            return Some(format!("_{}{}", base, name));
        }
    }
    None
}

/// Resolve a dotted path from `root`, trying mangled names for private
/// class members.  Err(AttributeError) on failure.
fn resolve_path(root: &PyObject, path: &[String]) -> PyResult<PyRef> {
    let mut cur = root.to_ref();
    for part in path {
        let nm = pystr::new_str(part);
        match ab::getattr(&cur, &nm) {
            Ok(v) => cur = v,
            Err(e) => {
                if !exc_matches(crate::types::exceptions::AttributeError()) {
                    return Err(e);
                }
                match mangle(&cur, part) {
                    Some(alt) => {
                        let saved = take_exc();
                        match ab::getattr_opt(&cur, &pystr::new_str(&alt))? {
                            Some(v) => cur = v,
                            None => {
                                if let Some(s) = saved {
                                    set_exc(s);
                                }
                                return Err(PyErr);
                            }
                        }
                    }
                    None => return Err(e),
                }
            }
        }
    }
    Ok(cur)
}

fn concrete_path(root: &PyObject, path: &[String]) -> PyResult<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = root.to_ref();
    for part in path {
        let mut use_name = part.clone();
        if ab::getattr_opt(&cur, &pystr::new_str(part))?.is_none() {
            if let Some(alt) = mangle(&cur, part) {
                use_name = alt;
            }
        }
        cur = ab::getattr(&cur, &pystr::new_str(&use_name))?;
        out.push(use_name);
    }
    Ok(out)
}

fn whichmodule(o: &PyObject, name: &PyObject, name_s: &str) -> PyResult<PyRef> {
    let path: Vec<String> = name_s.split('.').map(|s| s.to_string()).collect();
    if path.iter().any(|p| p == "<locals>") {
        let r = ab::repr_string(o)?;
        return Err(pickling_error(&format!("Can't pickle local object {}", r)));
    }
    let _ = name;
    let mut module_name = ab::getattr_opt(o, &pystr::intern_str("__module__"))?.filter(|m| !m.is(none::none()));
    if module_name.is_none() {
        let modules = crate::modules::sys::modules_dict();
        let snapshot = dict::items_vec(&modules);
        for (k, m) in snapshot {
            if m.is(none::none()) {
                continue;
            }
            let ks = pystr::to_string_lossy(&k);
            if ks == "__main__" || ks == "__mp_main__" {
                continue;
            }
            match resolve_path(&m, &path) {
                Ok(v) if v.is(o) => return Ok(k),
                Ok(_) => {}
                Err(_) => clear_exc(),
            }
        }
        module_name = Some(pystr::new_str("__main__"));
    }
    let module_name = module_name.unwrap();
    let imp = crate::builtins::builtin("__import__")?;
    let module = match ab::call(&imp, &[&module_name]).and_then(|_| {
        let modules = crate::modules::sys::modules_dict();
        match dict::get_item(&modules, &module_name)? {
            Some(m) => Ok(m),
            None => Err(crate::errors::set_err_obj(crate::types::exceptions::KeyError(), module_name.clone())),
        }
    }) {
        Ok(m) => m,
        Err(_) => {
            let is_expected = exc_matches(crate::types::exceptions::ImportError()) || exc_matches(crate::types::exceptions::ValueError()) || exc_matches(crate::types::exceptions::KeyError());
            if !is_expected {
                return Err(PyErr);
            }
            let ctx = take_exc();
            let msg = ctx.as_ref().map(|c| ab::str_string(c).unwrap_or_default()).unwrap_or_default();
            let r = ab::repr_string(o)?;
            return Err(raise_with_context(&globals().pickling_error, &format!("Can't pickle {}: {}", r, msg), ctx));
        }
    };
    let ms = pystr::to_string_lossy(&module_name);
    match resolve_path(&module, &path) {
        Ok(v) => {
            if v.is(o) {
                return Ok(module_name);
            }
            let r = ab::repr_string(o)?;
            Err(pickling_error(&format!("Can't pickle {}: it's not the same object as {}.{}", r, ms, name_s)))
        }
        Err(_) => {
            if !exc_matches(crate::types::exceptions::AttributeError()) {
                return Err(PyErr);
            }
            let ctx = take_exc();
            let r = ab::repr_string(o)?;
            Err(raise_with_context(&globals().pickling_error, &format!("Can't pickle {}: it's not found as {}.{}", r, ms, name_s), ctx))
        }
    }
}

fn check_pickler_ready(o: &PyObject) -> PyResult<&mut PState> {
    let st = pstate(o);
    if !st.initialized {
        return Err(pickling_error(&format!("Pickler.__init__() was not called by {}.__init__()", type_name(unsafe { &*o.type_ptr() }))));
    }
    Ok(st)
}

fn pickler_dump(o: &PyObject, obj: &PyObject) -> PyResult {
    let st = check_pickler_ready(o)?;
    if st.running {
        raise!(RuntimeError, "Pickler.dump() was called recursively");
    }
    if st.write.is_none() {
        raise!(TypeError, "file must have a 'write' attribute");
    }
    st.running = true;
    let stp: *mut PState = st;
    let r = P { obj: o, st: unsafe { &mut *stp } }.dump(obj);
    unsafe { (*stp).running = false };
    r?;
    Ok(none::none_ref())
}

fn pickler_clear_memo(o: &PyObject) -> PyResult {
    pstate(o).memo.map.clear();
    Ok(none::none_ref())
}

fn pickler_persistent_id(_o: &PyObject, _obj: &PyObject) -> PyResult {
    Ok(none::none_ref())
}

fn pickler_sizeof(o: &PyObject) -> PyResult {
    // Reported like the C pickler: a memo hash table (header + 16-byte entries,
    // starting at 8 slots and growing 4x when 2/3 full) and the write buffer.
    let st = pstate(o);
    let base = unsafe { (*o.type_ptr()).tp_basicsize } as usize;
    let mut slots = 8usize;
    for n in 1..=st.memo.map.len() {
        if n * 3 >= (slots - 1) * 2 {
            let want = if n > 50000 { n * 2 } else { n * 4 };
            while slots < want {
                slots <<= 1;
            }
        }
    }
    let mut size = base + 32 + slots * 16;
    if st.out.capacity() > 0 {
        size += unsafe { (*crate::types::bytes::bytes_type()).tp_basicsize } as usize + st.out.capacity();
    }
    Ok(int::new_int(size as i64))
}

fn int_attr(v: Option<&PyObject>) -> PyResult<i64> {
    let v = match v {
        None => raise!(TypeError, "can't delete numeric/char attribute"),
        Some(v) => v,
    };
    if !int::is_int(v) {
        if float::is_float(v) || !ab::has_index(v) {
            raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(v));
        }
    }
    let i = ab::index(v)?;
    match int::as_i64(&i) {
        Some(x) => Ok(x),
        None => raise!(OverflowError, "Python int too large to convert to C long"),
    }
}

fn get_bin(o: &PyObject) -> PyResult {
    Ok(int::new_int(pstate(o).bin as i64))
}
fn set_bin(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let x = int_attr(v)?;
    pstate(o).bin = x != 0;
    Ok(())
}
fn get_fast(o: &PyObject) -> PyResult {
    Ok(int::new_int(pstate(o).fast))
}
fn set_fast(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let x = int_attr(v)?;
    pstate(o).fast = x;
    Ok(())
}

// ----- memo proxies

#[repr(C)]
struct MemoProxyObj {
    ob_base: PyObject,
    owner: *mut PyObject,
}

fn new_proxy(tp: *mut PyTypeObject, owner: &PyObject) -> PyResult {
    unsafe {
        let op = generic_alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        (*(op as *mut MemoProxyObj)).owner = owner.to_ref().into_raw();
        Ok(PyRef::from_raw(op))
    }
}

unsafe extern "C" fn proxy_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        let p = &mut *(o as *mut MemoProxyObj);
        if !p.owner.is_null() {
            crate::object::decref(p.owner);
        }
        generic_free_and_type_decref(o, tp);
    }
}

fn proxy_owner(o: &PyObject) -> &PyObject {
    unsafe { &*(*(o.as_ptr() as *mut MemoProxyObj)).owner }
}

fn pmemo_copy(o: &PyObject) -> PyResult {
    let st = pstate(proxy_owner(o));
    let d = dict::new_dict();
    for (k, (idx, obj)) in st.memo.map.iter() {
        dict::set_item(&d, &int::new_int_usize(*k), &tuple::tuple2(int::new_int(*idx as i64), obj.clone()))?;
    }
    Ok(d)
}
fn pmemo_clear(o: &PyObject) -> PyResult {
    pstate(proxy_owner(o)).memo.map.clear();
    Ok(none::none_ref())
}
fn pmemo_reduce(o: &PyObject) -> PyResult {
    let c = pmemo_copy(o)?;
    Ok(tuple::tuple2(type_ref(dict::dict_type()), tuple::new_tuple(vec![c])))
}

fn get_pmemo(o: &PyObject) -> PyResult {
    new_proxy(pmemo_type(), o)
}

fn set_pmemo(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        None => raise!(TypeError, "attribute deletion is not supported"),
        Some(v) => v,
    };
    let mut map = HashMap::new();
    if v.is_type(pmemo_type()) {
        let src = pstate(proxy_owner(v));
        for (k, e) in src.memo.map.iter() {
            map.insert(*k, (e.0, e.1.clone()));
        }
    } else if dict::is_dict(v) {
        for (_, val) in dict::items_vec(v) {
            if !tuple::is_tuple(&val) || tuple::tuple_len(&val) != 2 {
                raise!(TypeError, "'memo' values must be 2-item tuples");
            }
            let idx = ab::index_as_ssize(tuple::tuple_get(&val, 0), None)?;
            let obj = tuple::tuple_get(&val, 1).to_ref();
            map.insert(obj.as_ptr() as usize, (idx as u32, obj));
        }
    } else {
        raise!(TypeError, "'memo' attribute must be a PicklerMemoProxy object or dict, not {}", tname(v));
    }
    pstate(o).memo.map = map;
    Ok(())
}

// ===================================================================== Unpickler

struct UState {
    initialized: bool,
    running: bool,
    read: Option<PyRef>,
    readline: Option<PyRef>,
    // input buffer: whole data for loads(), current frame for files
    buf: Vec<u8>,
    pos: usize,
    limit: usize,
    in_frame: bool,
    from_file: bool,
    stack: Vec<PyRef>,
    marks: Vec<usize>,
    memo: Vec<Option<PyRef>>,
    memo_sparse: HashMap<usize, PyRef>,
    memo_len: usize,
    /// table sizes the C unpickler would have allocated (for __sizeof__)
    memo_cap: usize,
    marks_cap: usize,
    line_copy: usize,
    proto: i64,
    encoding: String,
    errors: String,
    fix_imports: bool,
    buffers: Option<PyRef>,
}

impl UState {
    fn new() -> UState {
        UState {
            initialized: false,
            running: false,
            read: None,
            readline: None,
            buf: Vec::new(),
            pos: 0,
            limit: 0,
            in_frame: false,
            from_file: true,
            stack: Vec::new(),
            marks: Vec::new(),
            memo: Vec::new(),
            memo_sparse: HashMap::new(),
            memo_len: 0,
            memo_cap: 32,
            marks_cap: 0,
            line_copy: 0,
            proto: 0,
            encoding: "ASCII".into(),
            errors: "strict".into(),
            fix_imports: true,
            buffers: None,
        }
    }

    fn memo_get(&self, i: usize) -> Option<PyRef> {
        if i < self.memo.len() {
            return self.memo[i].clone();
        }
        self.memo_sparse.get(&i).cloned()
    }

    fn memo_put(&mut self, i: usize, v: PyRef) {
        while i >= self.memo_cap {
            self.memo_cap *= 2;
        }
        if i < self.memo.len() {
            if self.memo[i].is_none() {
                self.memo_len += 1;
            }
            self.memo[i] = Some(v);
            return;
        }
        if i <= self.memo.len() * 2 + 32 {
            self.memo.resize(i + 1, None);
            self.memo[i] = Some(v);
            self.memo_len += 1;
            return;
        }
        if self.memo_sparse.insert(i, v).is_none() {
            self.memo_len += 1;
        }
    }

    fn memo_clear(&mut self) {
        self.memo.clear();
        self.memo_sparse.clear();
        self.memo_len = 0;
    }
}

#[repr(C)]
struct UnpicklerObj {
    ob_base: PyObject,
    dict: *mut PyObject,
    weakreflist: *mut PyObject,
    st: *mut UState,
    /// unused: keeps the instance size of the C implementation (sys.getsizeof)
    _pad: [usize; 33],
}

fn ustate(o: &PyObject) -> &mut UState {
    unsafe {
        let p = &mut *(o.as_ptr() as *mut UnpicklerObj);
        if p.st.is_null() {
            p.st = Box::into_raw(Box::new(UState::new()));
        }
        &mut *p.st
    }
}

unsafe extern "C" fn unpickler_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let op = generic_alloc(tp, 0);
        if op.is_null() {
            return op;
        }
        (*(op as *mut UnpicklerObj)).st = Box::into_raw(Box::new(UState::new()));
        op
    }
}

unsafe extern "C" fn unpickler_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        let p = &mut *(o as *mut UnpicklerObj);
        if !p.weakreflist.is_null() {
            crate::types::weakref::clear_weakrefs(o);
        }
        if !p.st.is_null() {
            drop(Box::from_raw(p.st));
            p.st = std::ptr::null_mut();
        }
        if !p.dict.is_null() {
            crate::object::decref(p.dict);
            p.dict = std::ptr::null_mut();
        }
        generic_free_and_type_decref(o, tp);
    }
}

fn str_arg(v: Option<&PyObject>, default: &str, what: &str) -> PyResult<String> {
    match v {
        None => Ok(default.to_string()),
        Some(v) => {
            if !pystr::is_str(v) {
                raise!(TypeError, "Unpickler() argument '{}' must be str, not {}", what, tname(v));
            }
            Ok(pystr::to_string_lossy(v))
        }
    }
}

fn unpickler_setup(o: &PyObject, file: Option<&PyObject>, fix: Option<&PyObject>, enc: Option<&PyObject>, errs: Option<&PyObject>, buffers: Option<&PyObject>) -> PyResult<()> {
    let st = ustate(o);
    if st.running {
        raise!(RuntimeError, "Unpickler.__init__() was called while unpickling");
    }
    let fix = match fix {
        Some(f) => ab::is_true(f)?,
        None => true,
    };
    let encoding = str_arg(enc, "ASCII", "encoding")?;
    let errors = str_arg(errs, "strict", "errors")?;
    let bufs = match buffers {
        Some(b) if !b.is(none::none()) => Some(ab::get_iter(b)?),
        _ => None,
    };
    if let Some(f) = file {
        let rl = ab::getattr(f, name!("readline"))?;
        let rd = ab::getattr(f, name!("read"))?;
        st.readline = Some(rl);
        st.read = Some(rd);
        st.from_file = true;
    }
    st.fix_imports = fix;
    st.encoding = encoding;
    st.errors = errors;
    st.buffers = bufs;
    st.memo_clear();
    st.proto = 0;
    st.initialized = true;
    Ok(())
}

unsafe extern "C" fn unpickler_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let (pos, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = pos.iter().map(|x| &**x).collect();
            let p = crate::args::Params::new("Unpickler", &["file", "fix_imports", "encoding", "errors", "buffers"], 0, 1).kwonly(4);
            let [file, fix, enc, errs, bufs] = crate::args::parse::<5>(&p, &refs, kwn.as_deref())?;
            unpickler_setup(&*o, file, fix, enc, errs, bufs)
        })();
        int_into_c(r)
    }
}

struct U<'a> {
    obj: &'a PyObject,
    st: &'a mut UState,
    pers_load: Option<PyRef>,
}

fn truncated() -> PyErr {
    unpickling_error("pickle data was truncated")
}

fn frame_exhausted() -> PyErr {
    unpickling_error("pickle exhausted before end of frame")
}

impl<'a> U<'a> {
    // ----------------------------------------------------------- input

    fn file_read(&mut self, n: usize) -> PyResult<Vec<u8>> {
        let rd = self.st.read.clone().unwrap();
        let mut out = Vec::new();
        let mut left = n;
        while left > 0 {
            let want = left.min(READ_PIECE);
            let data = ab::call(&rd, &[&int::new_int(want as i64)])?;
            let chunk = match crate::buffer::buffer_bytes(&data) {
                Some(b) => b,
                None => raise!(TypeError, "read() should return bytes, not {}", tname(&data)),
            };
            let got = chunk.len();
            out.extend_from_slice(&chunk);
            if got < want {
                break;
            }
            left -= got;
        }
        Ok(out)
    }

    fn read_opcode(&mut self) -> PyResult<Option<u8>> {
        if self.st.in_frame {
            if self.st.pos < self.st.limit {
                let b = self.st.buf[self.st.pos];
                self.st.pos += 1;
                return Ok(Some(b));
            }
            self.st.in_frame = false;
            if self.st.from_file {
                self.st.buf.clear();
                self.st.pos = 0;
                self.st.limit = 0;
            } else {
                self.st.limit = self.st.buf.len();
            }
        }
        if !self.st.from_file {
            if self.st.pos < self.st.limit {
                let b = self.st.buf[self.st.pos];
                self.st.pos += 1;
                return Ok(Some(b));
            }
            return Ok(None);
        }
        let d = self.file_read(1)?;
        Ok(d.first().copied())
    }

    fn read(&mut self, n: usize) -> PyResult<Vec<u8>> {
        if self.st.in_frame || !self.st.from_file {
            if n > self.st.limit - self.st.pos {
                return Err(if self.st.in_frame { frame_exhausted() } else { truncated() });
            }
            let s = self.st.pos;
            self.st.pos += n;
            return Ok(self.st.buf[s..s + n].to_vec());
        }
        let d = self.file_read(n)?;
        if d.len() < n {
            return Err(truncated());
        }
        Ok(d)
    }

    fn read1(&mut self) -> PyResult<u8> {
        if (self.st.in_frame || !self.st.from_file) && self.st.pos < self.st.limit {
            let b = self.st.buf[self.st.pos];
            self.st.pos += 1;
            return Ok(b);
        }
        Ok(self.read(1)?[0])
    }

    fn read_u32(&mut self) -> PyResult<u32> {
        let d = self.read(4)?;
        Ok(u32::from_le_bytes([d[0], d[1], d[2], d[3]]))
    }

    fn read_u64(&mut self) -> PyResult<u64> {
        let d = self.read(8)?;
        Ok(u64::from_le_bytes(d[..8].try_into().unwrap()))
    }

    /// One line including the trailing newline.
    fn readline(&mut self) -> PyResult<Vec<u8>> {
        if self.st.in_frame || !self.st.from_file {
            let start = self.st.pos;
            let end = self.st.limit;
            match self.st.buf[start..end].iter().position(|&b| b == b'\n') {
                Some(i) => {
                    self.st.pos = start + i + 1;
                    return Ok(self.st.buf[start..start + i + 1].to_vec());
                }
                None => return Err(if self.st.in_frame { frame_exhausted() } else { truncated() }),
            }
        }
        let rl = self.st.readline.clone().unwrap();
        let line = ab::call(&rl, &[])?;
        let data = match crate::buffer::buffer_bytes(&line) {
            Some(b) => b,
            None => raise!(TypeError, "readline() should return bytes, not {}", tname(&line)),
        };
        if !data.ends_with(b"\n") {
            return Err(truncated());
        }
        // the C unpickler keeps a NUL-terminated copy of the last line read from a file
        self.st.line_copy = data.len() + 1;
        Ok(data)
    }

    fn line_body(&mut self) -> PyResult<Vec<u8>> {
        let mut l = self.readline()?;
        l.pop();
        Ok(l)
    }

    fn load_frame(&mut self) -> PyResult<()> {
        let size = self.read_u64()?;
        if size > isize::MAX as u64 {
            raise!(OverflowError, "FRAME length exceeds system's maximum of {} bytes", isize::MAX);
        }
        let size = size as usize;
        if self.st.in_frame && self.st.pos < self.st.limit {
            return Err(unpickling_error("beginning of a new frame before end of current frame"));
        }
        if !self.st.from_file {
            if size > self.st.buf.len() - self.st.pos {
                return Err(truncated());
            }
            self.st.limit = self.st.pos + size;
            self.st.in_frame = true;
            return Ok(());
        }
        let data = self.file_read(size)?;
        if data.len() < size {
            return Err(truncated());
        }
        self.st.buf = data;
        self.st.pos = 0;
        self.st.limit = size;
        self.st.in_frame = true;
        Ok(())
    }

    // ----------------------------------------------------------- stack

    fn floor(&self) -> usize {
        self.st.marks.last().copied().unwrap_or(0)
    }

    fn push(&mut self, v: PyRef) {
        self.st.stack.push(v);
    }

    fn pop(&mut self) -> PyResult<PyRef> {
        if self.st.stack.len() <= self.floor() {
            return Err(unpickling_error("unpickling stack underflow"));
        }
        Ok(self.st.stack.pop().unwrap())
    }

    fn top(&self) -> PyResult<PyRef> {
        if self.st.stack.len() <= self.floor() {
            return Err(unpickling_error("unpickling stack underflow"));
        }
        Ok(self.st.stack.last().unwrap().clone())
    }

    fn pop_mark(&mut self) -> PyResult<Vec<PyRef>> {
        let m = match self.st.marks.pop() {
            Some(m) => m,
            None => return Err(unpickling_error("could not find MARK")),
        };
        Ok(self.st.stack.split_off(m))
    }

    // ----------------------------------------------------------- helpers

    fn decode_string(&self, data: &[u8]) -> PyResult {
        let b = bytes::new_bytes(data);
        if self.st.encoding == "bytes" {
            return Ok(b);
        }
        ab::call_method(&b, name!("decode"), &[&pystr::new_str(&self.st.encoding), &pystr::new_str(&self.st.errors)])
    }

    fn find_class(&mut self, module: &PyObject, name: &PyObject) -> PyResult {
        let f = ab::getattr(self.obj, &pystr::intern_str("find_class"))?;
        ab::call(&f, &[module, name])
    }

    fn get_extension(&mut self, code: i64) -> PyResult<()> {
        if code <= 0 {
            return Err(unpickling_error("EXT specifies code <= 0"));
        }
        let cr = import("copyreg")?;
        let cache = ab::getattr_str(&cr, "_extension_cache")?;
        let key = int::new_int(code);
        if let Some(v) = ab::call_method(&cache, name!("get"), &[&key])
            .ok()
            .filter(|v| !v.is(none::none()))
        {
            self.push(v);
            return Ok(());
        }
        clear_exc();
        let inv = ab::getattr_str(&cr, "_inverted_registry")?;
        let pair = ab::call_method(&inv, name!("get"), &[&key])?;
        if pair.is(none::none()) || !ab::is_true(&pair)? {
            raise!(ValueError, "unregistered extension code {}", code);
        }
        if !tuple::is_tuple(&pair) || tuple::tuple_len(&pair) != 2 {
            raise!(ValueError, "_inverted_registry[{}] isn't a 2-tuple of strings", code);
        }
        let obj = self.find_class(tuple::tuple_get(&pair, 0), tuple::tuple_get(&pair, 1))?;
        ab::setitem(&cache, &key, Some(&obj))?;
        self.push(obj);
        Ok(())
    }

    fn instantiate(&mut self, klass: &PyObject, args: Vec<PyRef>) -> PyResult<()> {
        let refs: Vec<&PyObject> = args.iter().map(|a| &**a).collect();
        let needs_call = !args.is_empty() || !is_type(klass) || ab::hasattr(klass, &pystr::intern_str("__getinitargs__"))?;
        let v = if needs_call {
            ab::call(klass, &refs)?
        } else {
            let new = ab::getattr(klass, name!("__new__"))?;
            ab::call(&new, &[klass])?
        };
        self.push(v);
        Ok(())
    }

    fn memo_fetch(&mut self, i: usize) -> PyResult<()> {
        match self.st.memo_get(i) {
            Some(v) => {
                self.push(v);
                Ok(())
            }
            None => Err(unpickling_error(&format!("Memo value not found at index {}", i))),
        }
    }

    fn build(&mut self) -> PyResult<()> {
        let state = self.pop()?;
        let inst = self.top()?;
        if let Some(setstate) = ab::getattr_opt(&inst, &pystr::intern_str("__setstate__"))? {
            ab::call(&setstate, &[&state])?;
            return Ok(());
        }
        let (state, slotstate) = if tuple::is_tuple(&state) && tuple::tuple_len(&state) == 2 {
            (tuple::tuple_get(&state, 0).to_ref(), Some(tuple::tuple_get(&state, 1).to_ref()))
        } else {
            (state, None)
        };
        if !state.is(none::none()) {
            if !dict::is_dict(&state) {
                return Err(unpickling_error("state is not a dictionary"));
            }
            if dict::dict_len(&state) > 0 {
                let target = ab::getattr(&inst, name!("__dict__"))?;
                for (k, v) in dict::items_vec(&state) {
                    let k = if pystr::is_str_exact(&k) { pystr::intern(k) } else { k };
                    ab::setitem(&target, &k, Some(&v))?;
                }
            }
        }
        if let Some(ss) = slotstate.filter(|s| !s.is(none::none())) {
            if !dict::is_dict(&ss) {
                return Err(unpickling_error("slot state is not a dictionary"));
            }
            for (k, v) in dict::items_vec(&ss) {
                ab::setattr(&inst, &k, &v)?;
            }
        }
        Ok(())
    }

    // ----------------------------------------------------------- main loop

    fn load(&mut self) -> PyResult {
        self.st.stack.clear();
        self.st.marks.clear();
        self.st.proto = 0;
        self.st.in_frame = false;
        if self.st.from_file {
            self.st.buf.clear();
            self.st.pos = 0;
            self.st.limit = 0;
        }
        loop {
            let opc = match self.read_opcode()? {
                Some(c) => c,
                None => raise!(EOFError, "Ran out of input"),
            };
            if opc == op::STOP {
                let v = self.pop()?;
                return Ok(v);
            }
            self.dispatch(opc)?;
        }
    }

    fn dispatch(&mut self, opc: u8) -> PyResult<()> {
        match opc {
            op::PROTO => {
                let p = self.read1()? as i64;
                if p > HIGHEST_PROTOCOL {
                    raise!(ValueError, "unsupported pickle protocol: {}", p);
                }
                self.st.proto = p;
            }
            op::FRAME => self.load_frame()?,
            op::NONE => self.push(none::none_ref()),
            op::NEWTRUE => self.push(bool_::true_obj().to_ref()),
            op::NEWFALSE => self.push(bool_::false_obj().to_ref()),
            op::INT => {
                let line = self.readline()?;
                let v = if line == b"00\n" {
                    bool_::false_obj().to_ref()
                } else if line == b"01\n" {
                    bool_::true_obj().to_ref()
                } else {
                    crate::types::int_methods::parse_int_obj(&bytes::new_bytes(&line), 10)?
                };
                self.push(v);
            }
            op::LONG => {
                let mut body = self.line_body()?;
                if body.last() == Some(&b'L') {
                    body.pop();
                }
                let v = crate::types::int_methods::parse_int_obj(&bytes::new_bytes(&body), 10)?;
                self.push(v);
            }
            op::BININT => {
                let v = self.read_u32()? as i32;
                self.push(int::new_int(v as i64));
            }
            op::BININT1 => {
                let v = self.read1()?;
                self.push(int::new_int(v as i64));
            }
            op::BININT2 => {
                let d = self.read(2)?;
                self.push(int::new_int(u16::from_le_bytes([d[0], d[1]]) as i64));
            }
            op::LONG1 => {
                let n = self.read1()? as usize;
                let d = self.read(n)?;
                self.push(decode_long(&d));
            }
            op::LONG4 => {
                let n = self.read_u32()? as i32;
                if n < 0 {
                    return Err(unpickling_error("LONG pickle has negative byte count"));
                }
                let d = self.read(n as usize)?;
                self.push(decode_long(&d));
            }
            op::FLOAT => {
                let body = self.line_body()?;
                let s = pystr::from_latin1(&body);
                let v = ab::call(type_obj(float::float_type()), &[&s])?;
                self.push(v);
            }
            op::BINFLOAT => {
                let d = self.read(8)?;
                self.push(float::new_float(f64::from_be_bytes(d[..8].try_into().unwrap())));
            }
            op::STRING => {
                let body = self.line_body()?;
                if body.len() < 2 || body[0] != body[body.len() - 1] || (body[0] != b'"' && body[0] != b'\'') {
                    return Err(unpickling_error("the STRING opcode argument must be quoted"));
                }
                let codecs = import("_codecs")?;
                let f = ab::getattr_str(&codecs, "escape_decode")?;
                let r = ab::call(&f, &[&bytes::new_bytes(&body[1..body.len() - 1])])?;
                let raw = tuple::tuple_get(&r, 0).to_ref();
                let v = self.decode_string(bytes::bytes_data(&raw))?;
                self.push(v);
            }
            op::BINSTRING => {
                let n = self.read_u32()? as i32;
                if n < 0 {
                    return Err(unpickling_error("BINSTRING pickle has negative byte count"));
                }
                let d = self.read(n as usize)?;
                let v = self.decode_string(&d)?;
                self.push(v);
            }
            op::SHORT_BINSTRING => {
                let n = self.read1()? as usize;
                let d = self.read(n)?;
                let v = self.decode_string(&d)?;
                self.push(v);
            }
            op::BINBYTES => {
                let n = self.read_u32()? as usize;
                let d = self.read(n)?;
                self.push(bytes::new_bytes(&d));
            }
            op::SHORT_BINBYTES => {
                let n = self.read1()? as usize;
                let d = self.read(n)?;
                self.push(bytes::new_bytes(&d));
            }
            op::BINBYTES8 => {
                let n = self.size8("BINBYTES8")?;
                let d = self.read(n)?;
                self.push(bytes::new_bytes(&d));
            }
            op::BYTEARRAY8 => {
                let n = self.size8("BYTEARRAY8")?;
                let d = self.read(n)?;
                let v = bytearray::new_bytearray(&d)?;
                self.push(v);
            }
            op::NEXT_BUFFER => {
                let bufs = match self.st.buffers.clone() {
                    Some(b) => b,
                    None => return Err(unpickling_error("pickle stream refers to out-of-band data but no *buffers* argument was given")),
                };
                match ab::iter_next(&bufs)? {
                    Some(b) => self.push(b),
                    None => return Err(unpickling_error("not enough out-of-band buffers")),
                }
            }
            op::READONLY_BUFFER => {
                let b = self.top()?;
                let m = crate::types::memoryview::from_object(&b)?;
                let ro = ab::getattr_str(&m, "readonly")?;
                if !ab::is_true(&ro)? {
                    let r = ab::call_method(&m, name!("toreadonly"), &[])?;
                    let n = self.st.stack.len();
                    self.st.stack[n - 1] = r;
                }
                let _ = ab::call_method(&m, name!("release"), &[]);
            }
            op::UNICODE => {
                let body = self.line_body()?;
                let b = bytes::new_bytes(&body);
                let v = ab::call(type_obj(pystr::str_type()), &[&b, &pystr::new_str("raw-unicode-escape")])?;
                self.push(v);
            }
            op::BINUNICODE => {
                let n = self.read_u32()? as usize;
                let d = self.read(n)?;
                let v = decode_utf8_surrogatepass(&d)?;
                self.push(v);
            }
            op::SHORT_BINUNICODE => {
                let n = self.read1()? as usize;
                let d = self.read(n)?;
                let v = decode_utf8_surrogatepass(&d)?;
                self.push(v);
            }
            op::BINUNICODE8 => {
                let n = self.size8("BINUNICODE8")?;
                let d = self.read(n)?;
                let v = decode_utf8_surrogatepass(&d)?;
                self.push(v);
            }
            op::EMPTY_TUPLE => self.push(tuple::empty_tuple()),
            op::TUPLE => {
                let items = self.pop_mark()?;
                self.push(tuple::new_tuple(items));
            }
            op::TUPLE1 | op::TUPLE2 | op::TUPLE3 => {
                let n = (opc - op::TUPLE1 + 1) as usize;
                if self.st.stack.len() < self.floor() + n {
                    return Err(unpickling_error("unpickling stack underflow"));
                }
                let at = self.st.stack.len() - n;
                let items = self.st.stack.split_off(at);
                self.push(tuple::new_tuple(items));
            }
            op::EMPTY_LIST => self.push(list::new_list_empty()),
            op::LIST => {
                let items = self.pop_mark()?;
                self.push(list::new_list(items));
            }
            op::EMPTY_DICT => self.push(dict::new_dict()),
            op::DICT => {
                let items = self.pop_mark()?;
                if items.len() % 2 != 0 {
                    return Err(unpickling_error("odd number of items for DICT"));
                }
                let d = dict::new_dict_presized(items.len() / 2);
                for pair in items.chunks(2) {
                    dict::set_item(&d, &pair[0], &pair[1])?;
                }
                self.push(d);
            }
            op::EMPTY_SET => {
                let s = set::make_set_empty()?;
                self.push(s);
            }
            op::FROZENSET => {
                let items = self.pop_mark()?;
                let l = list::new_list(items);
                let fs = ab::call(type_obj(set::frozenset_type()), &[&l])?;
                self.push(fs);
            }
            op::POP => {
                if self.st.stack.len() > self.floor() {
                    self.st.stack.pop();
                } else if !self.st.marks.is_empty() {
                    self.pop_mark()?;
                } else {
                    return Err(unpickling_error("unpickling stack underflow"));
                }
            }
            op::POP_MARK => {
                self.pop_mark()?;
            }
            op::DUP => {
                let t = self.top()?;
                self.push(t);
            }
            op::MARK => {
                let n = self.st.stack.len();
                self.st.marks.push(n);
                if self.st.marks.len() > self.st.marks_cap {
                    self.st.marks_cap = (self.st.marks_cap << 1) + 20;
                }
            }
            op::GET => {
                let body = self.line_body()?;
                let v = crate::types::int_methods::parse_int_obj(&bytes::new_bytes(&body), 10)?;
                let i = match int::as_i64(&v) {
                    Some(i) if i >= 0 => i as usize,
                    _ => return Err(unpickling_error(&format!("Memo value not found at index {}", pystr::to_string_lossy(&*ab::str(&v)?)))),
                };
                self.memo_fetch(i)?;
            }
            op::BINGET => {
                let i = self.read1()? as usize;
                self.memo_fetch(i)?;
            }
            op::LONG_BINGET => {
                let i = self.read_u32()? as usize;
                self.memo_fetch(i)?;
            }
            op::PUT => {
                let body = self.line_body()?;
                let v = crate::types::int_methods::parse_int_obj(&bytes::new_bytes(&body), 10)?;
                let i = match int::as_i64(&v) {
                    Some(i) if i < 0 => raise!(ValueError, "negative PUT argument"),
                    Some(i) => i as usize,
                    None => raise!(OverflowError, "memo key too large"),
                };
                let t = self.top()?;
                self.st.memo_put(i, t);
            }
            op::BINPUT => {
                let i = self.read1()? as usize;
                let t = self.top()?;
                self.st.memo_put(i, t);
            }
            op::LONG_BINPUT => {
                let i = self.read_u32()? as usize;
                let t = self.top()?;
                self.st.memo_put(i, t);
            }
            op::MEMOIZE => {
                let t = self.top()?;
                let i = self.st.memo_len;
                self.st.memo_put(i, t);
            }
            op::APPEND => {
                let v = self.pop()?;
                let l = self.top()?;
                if list::is_list_exact(&l) {
                    list::append(&l, v);
                } else {
                    ab::call_method(&l, name!("append"), &[&v])?;
                }
            }
            op::APPENDS => {
                let items = self.pop_mark()?;
                let l = self.top()?;
                if list::is_list_exact(&l) {
                    for v in items {
                        list::append(&l, v);
                    }
                } else if let Some(ext) = ab::getattr_opt(&l, &pystr::intern_str("extend"))? {
                    ab::call(&ext, &[&list::new_list(items)])?;
                } else {
                    let app = ab::getattr(&l, name!("append"))?;
                    for v in items {
                        ab::call(&app, &[&v])?;
                    }
                }
            }
            op::SETITEM => {
                let v = self.pop()?;
                let k = self.pop()?;
                let d = self.top()?;
                ab::setitem(&d, &k, Some(&v))?;
            }
            op::SETITEMS => {
                let items = self.pop_mark()?;
                let d = self.top()?;
                if items.len() % 2 != 0 {
                    return Err(unpickling_error("odd number of items for SETITEMS"));
                }
                for pair in items.chunks(2) {
                    ab::setitem(&d, &pair[0], Some(&pair[1]))?;
                }
            }
            op::ADDITEMS => {
                let items = self.pop_mark()?;
                let s = self.top()?;
                if s.is_type(set::set_type()) {
                    ab::call_method(&s, name!("update"), &[&list::new_list(items)])?;
                } else {
                    let add = ab::getattr(&s, name!("add"))?;
                    for v in items {
                        ab::call(&add, &[&v])?;
                    }
                }
            }
            op::BUILD => self.build()?,
            op::GLOBAL => {
                let m = self.line_body()?;
                let n = self.line_body()?;
                if m.is_empty() || n.is_empty() {
                    return Err(truncated());
                }
                let ms = decode_utf8_strict(&m)?;
                let ns = decode_utf8_strict(&n)?;
                let v = self.find_class(&ms, &ns)?;
                self.push(v);
            }
            op::STACK_GLOBAL => {
                let name = self.pop()?;
                let module = self.pop()?;
                if !pystr::is_str_exact(&name) || !pystr::is_str_exact(&module) {
                    return Err(unpickling_error("STACK_GLOBAL requires str"));
                }
                let v = self.find_class(&module, &name)?;
                self.push(v);
            }
            op::INST => {
                let m = self.line_body()?;
                let n = self.line_body()?;
                if m.is_empty() || n.is_empty() {
                    return Err(truncated());
                }
                let ms = decode_ascii_strict(&m)?;
                let ns = decode_ascii_strict(&n)?;
                let klass = self.find_class(&ms, &ns)?;
                let args = self.pop_mark()?;
                self.instantiate(&klass, args)?;
            }
            op::OBJ => {
                let mut args = self.pop_mark()?;
                if args.is_empty() {
                    return Err(unpickling_error("unpickling stack underflow"));
                }
                let klass = args.remove(0);
                self.instantiate(&klass, args)?;
            }
            op::NEWOBJ => {
                let args = self.pop()?;
                let cls = self.pop()?;
                if !tuple::is_tuple(&args) {
                    return Err(unpickling_error("NEWOBJ expected an arg tuple."));
                }
                if !is_type(&cls) {
                    return Err(unpickling_error("NEWOBJ class argument isn't a type object"));
                }
                let new = ab::getattr(&cls, name!("__new__"))?;
                let mut call: Vec<&PyObject> = vec![&cls];
                let items: Vec<PyRef> = tuple::tuple_to_vec(&args);
                call.extend(items.iter().map(|x| &**x));
                let v = ab::call(&new, &call)?;
                self.push(v);
            }
            op::NEWOBJ_EX => {
                let kwargs = self.pop()?;
                let args = self.pop()?;
                let cls = self.pop()?;
                if !is_type(&cls) {
                    return Err(unpickling_error(&format!("NEWOBJ_EX class argument must be a type, not {}", tname(&cls))));
                }
                if !tuple::is_tuple(&args) {
                    return Err(unpickling_error(&format!("NEWOBJ_EX args argument must be a tuple, not {}", tname(&args))));
                }
                if !dict::is_dict(&kwargs) {
                    return Err(unpickling_error(&format!("NEWOBJ_EX kwargs argument must be a dict, not {}", tname(&kwargs))));
                }
                let new = ab::getattr(&cls, name!("__new__"))?;
                let mut call: Vec<&PyObject> = vec![&cls];
                let items: Vec<PyRef> = tuple::tuple_to_vec(&args);
                call.extend(items.iter().map(|x| &**x));
                let v = ab::call_with_kwargs_dict(&new, &call, Some(&kwargs))?;
                self.push(v);
            }
            op::REDUCE => {
                let args = self.pop()?;
                let f = self.pop()?;
                if !tuple::is_tuple(&args) {
                    raise!(TypeError, "argument list must be a tuple, not {}", tname(&args));
                }
                let items: Vec<PyRef> = tuple::tuple_to_vec(&args);
                let refs: Vec<&PyObject> = items.iter().map(|x| &**x).collect();
                let v = ab::call(&f, &refs)?;
                self.push(v);
            }
            op::EXT1 => {
                let c = self.read1()? as i64;
                self.get_extension(c)?;
            }
            op::EXT2 => {
                let d = self.read(2)?;
                self.get_extension(u16::from_le_bytes([d[0], d[1]]) as i64)?;
            }
            op::EXT4 => {
                let c = self.read_u32()? as i32;
                self.get_extension(c as i64)?;
            }
            op::PERSID => {
                let body = self.line_body()?;
                if !body.is_ascii() {
                    return Err(unpickling_error("persistent IDs in protocol 0 must be ASCII strings"));
                }
                let pid = pystr::from_latin1(&body);
                let v = self.call_persistent_load(&pid)?;
                self.push(v);
            }
            op::BINPERSID => {
                let pid = self.pop()?;
                let v = self.call_persistent_load(&pid)?;
                self.push(v);
            }
            _ => {
                let c = opc as char;
                if opc >= 0x20 && opc < 0x7f {
                    return Err(unpickling_error(&format!("invalid load key, '{}'.", c)));
                }
                return Err(unpickling_error(&format!("invalid load key, '\\x{:02x}'.", opc)));
            }
        }
        Ok(())
    }

    fn size8(&mut self, what: &str) -> PyResult<usize> {
        let n = self.read_u64()?;
        if n > isize::MAX as u64 {
            raise!(OverflowError, "{} exceeds system's maximum size of {} bytes", what, isize::MAX);
        }
        Ok(n as usize)
    }

    fn call_persistent_load(&mut self, pid: &PyObject) -> PyResult {
        let f = match self.pers_load.clone() {
            Some(f) => f,
            None => {
                let f = ab::getattr(self.obj, &pystr::intern_str("persistent_load"))?;
                self.pers_load = Some(f.clone());
                f
            }
        };
        ab::call(&f, &[pid])
    }
}

fn decode_utf8_strict(d: &[u8]) -> PyResult {
    match std::str::from_utf8(d) {
        Ok(s) => Ok(pystr::new_str(s)),
        Err(_) => ab::call_method(&bytes::new_bytes(d), name!("decode"), &[&pystr::new_str("utf-8")]),
    }
}

fn decode_ascii_strict(d: &[u8]) -> PyResult {
    if d.is_ascii() {
        return Ok(pystr::from_latin1(d));
    }
    ab::call_method(&bytes::new_bytes(d), name!("decode"), &[&pystr::new_str("ascii")])
}

fn run_load(o: &PyObject) -> PyResult {
    let st = ustate(o);
    if !st.initialized {
        return Err(unpickling_error(&format!("Unpickler.__init__() was not called by {}.__init__()", type_name(unsafe { &*o.type_ptr() }))));
    }
    if st.running {
        raise!(RuntimeError, "Unpickler.load() was called recursively");
    }
    st.running = true;
    let stp: *mut UState = st;
    let r = U { obj: o, st: unsafe { &mut *stp }, pers_load: None }.load();
    unsafe {
        (*stp).running = false;
        (*stp).stack.clear();
        (*stp).marks.clear();
    }
    r
}

fn unpickler_load(o: &PyObject) -> PyResult {
    run_load(o)
}

fn unpickler_find_class(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("find_class", args, 2, 2)?;
    let (mut module, mut name) = (args[0].to_ref(), args[1].to_ref());
    let audit = crate::modules::sys::modules_dict();
    let _ = audit;
    if let Ok(sysm) = import("sys") {
        if let Ok(a) = ab::getattr_str(&sysm, "audit") {
            ab::call(&a, &[&pystr::new_str("pickle.find_class"), &module, &name])?;
        }
    }
    let st = ustate(o);
    if st.proto < 3 && st.fix_imports {
        let cp = import("_compat_pickle")?;
        let nm = ab::getattr_str(&cp, "NAME_MAPPING")?;
        let im = ab::getattr_str(&cp, "IMPORT_MAPPING")?;
        let key = tuple::tuple2(module.clone(), name.clone());
        if let Some(v) = dict::get_item(&nm, &key).unwrap_or_else(|_| {
            clear_exc();
            None
        }) {
            if !tuple::is_tuple(&v) || tuple::tuple_len(&v) != 2 {
                raise!(RuntimeError, "_compat_pickle.NAME_MAPPING values should be 2-tuples, not {}", tname(&v));
            }
            module = tuple::tuple_get(&v, 0).to_ref();
            name = tuple::tuple_get(&v, 1).to_ref();
        } else if let Some(v) = dict::get_item(&im, &module).unwrap_or_else(|_| {
            clear_exc();
            None
        }) {
            module = v;
        }
    }
    let imp = crate::builtins::builtin("__import__")?;
    ab::call(&imp, &[&module])?;
    let modules = crate::modules::sys::modules_dict();
    let m = match dict::get_item(&modules, &module)? {
        Some(m) => m,
        None => return Err(crate::errors::set_err_obj(crate::types::exceptions::KeyError(), module)),
    };
    if !pystr::is_str(&name) {
        return ab::getattr(&m, &name);
    }
    let ns = pystr::to_string_lossy(&name);
    if st.proto >= 4 && ns.contains('.') {
        let path: Vec<String> = ns.split('.').map(|s| s.to_string()).collect();
        return match resolve_path(&m, &path) {
            Ok(v) => Ok(v),
            Err(e) => {
                if !exc_matches(crate::types::exceptions::AttributeError()) {
                    return Err(e);
                }
                let ctx = take_exc();
                let nr = ab::repr_string(&name)?;
                let mr = ab::repr_string(&module)?;
                Err(raise_with_context(type_obj(crate::types::exceptions::AttributeError()), &format!("Can't resolve path {} on module {}", nr, mr), ctx))
            }
        };
    }
    ab::getattr(&m, &name)
}

fn unpickler_persistent_load(_o: &PyObject, _pid: &PyObject) -> PyResult {
    Err(unpickling_error("A load persistent id instruction was encountered, but no persistent_load function was specified."))
}

fn unpickler_sizeof(o: &PyObject) -> PyResult {
    let st = ustate(o);
    let base = unsafe { (*o.type_ptr()).tp_basicsize } as usize;
    Ok(int::new_int((base + st.memo_cap * 8 + st.marks_cap * 8 + st.line_copy + st.encoding.len() + 1 + st.errors.len() + 1) as i64))
}

fn umemo_copy(o: &PyObject) -> PyResult {
    let st = ustate(proxy_owner(o));
    let d = dict::new_dict();
    for (i, v) in st.memo.iter().enumerate() {
        if let Some(v) = v {
            dict::set_item(&d, &int::new_int(i as i64), v)?;
        }
    }
    for (i, v) in st.memo_sparse.iter() {
        dict::set_item(&d, &int::new_int(*i as i64), v)?;
    }
    Ok(d)
}
fn umemo_clear(o: &PyObject) -> PyResult {
    ustate(proxy_owner(o)).memo_clear();
    Ok(none::none_ref())
}
fn umemo_reduce(o: &PyObject) -> PyResult {
    let c = umemo_copy(o)?;
    Ok(tuple::tuple2(type_ref(dict::dict_type()), tuple::new_tuple(vec![c])))
}

fn get_umemo(o: &PyObject) -> PyResult {
    new_proxy(umemo_type(), o)
}

fn set_umemo(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        None => raise!(TypeError, "attribute deletion is not supported"),
        Some(v) => v,
    };
    let mut entries: Vec<(usize, PyRef)> = Vec::new();
    if v.is_type(umemo_type()) {
        let src = ustate(proxy_owner(v));
        for (i, x) in src.memo.iter().enumerate() {
            if let Some(x) = x {
                entries.push((i, x.clone()));
            }
        }
        for (i, x) in src.memo_sparse.iter() {
            entries.push((*i, x.clone()));
        }
    } else if dict::is_dict(v) {
        for (k, val) in dict::items_vec(v) {
            if !int::is_int(&k) {
                raise!(TypeError, "memo key must be integers");
            }
            let i = match int::as_i64(&k) {
                Some(i) if i >= 0 => i as usize,
                _ => raise!(ValueError, "memo key must be positive integers."),
            };
            entries.push((i, val));
        }
    } else {
        raise!(TypeError, "'memo' attribute must be an UnpicklerMemoProxy object or dict, not {}", tname(v));
    }
    let st = ustate(o);
    st.memo_clear();
    for (i, x) in entries {
        st.memo_put(i, x);
    }
    Ok(())
}

// ===================================================================== module functions

fn dumps_impl(obj: &PyObject, protocol: Option<&PyObject>, fix: Option<&PyObject>, cb: Option<&PyObject>, file: Option<&PyObject>) -> PyResult {
    let p = unsafe { PyRef::from_raw(pickler_new(pickler_type(), std::ptr::null_mut(), std::ptr::null_mut())) };
    if p.as_ptr().is_null() {
        return Err(PyErr);
    }
    pickler_setup(&p, file, protocol, fix, cb)?;
    let st = pstate(&p);
    st.running = true;
    let stp: *mut PState = st;
    let r = P { obj: &p, st: unsafe { &mut *stp } }.dump(obj);
    unsafe { (*stp).running = false };
    r?;
    if file.is_some() {
        return Ok(none::none_ref());
    }
    Ok(bytes::new_bytes(&unsafe { &*stp }.out))
}

fn m_dump(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let p = crate::args::Params::new("dump", &["obj", "file", "protocol", "fix_imports", "buffer_callback"], 0, 2).kwonly(2);
    let [obj, file, proto, fix, cb] = crate::args::parse::<5>(&p, args, kw)?;
    dumps_impl(obj.unwrap(), proto, fix, cb, file)
}

fn m_dumps(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let p = crate::args::Params::new("dumps", &["obj", "protocol", "fix_imports", "buffer_callback"], 0, 1).kwonly(2);
    let [obj, proto, fix, cb] = crate::args::parse::<4>(&p, args, kw)?;
    dumps_impl(obj.unwrap(), proto, fix, cb, None)
}

fn new_unpickler() -> PyResult {
    let u = unsafe { PyRef::from_raw(unpickler_new(unpickler_type(), std::ptr::null_mut(), std::ptr::null_mut())) };
    if u.as_ptr().is_null() {
        return Err(PyErr);
    }
    Ok(u)
}

fn m_load(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let p = crate::args::Params::new("load", &["file", "fix_imports", "encoding", "errors", "buffers"], 0, 1).kwonly(4);
    let [file, fix, enc, errs, bufs] = crate::args::parse::<5>(&p, args, kw)?;
    let u = new_unpickler()?;
    unpickler_setup(&u, file, fix, enc, errs, bufs)?;
    run_load(&u)
}

fn m_loads(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let p = crate::args::Params::new("loads", &["data", "fix_imports", "encoding", "errors", "buffers"], 1, 1).kwonly(4);
    let [data, fix, enc, errs, bufs] = crate::args::parse::<5>(&p, args, kw)?;
    let data = data.unwrap();
    if pystr::is_str(data) {
        raise!(TypeError, "a bytes-like object is required, not 'str'");
    }
    let bytes_in = match crate::buffer::buffer_bytes(data) {
        Some(b) => b,
        None => raise!(TypeError, "a bytes-like object is required, not '{}'", tname(data)),
    };
    let u = new_unpickler()?;
    unpickler_setup(&u, None, fix, enc, errs, bufs)?;
    let st = ustate(&u);
    st.from_file = false;
    st.limit = bytes_in.len();
    st.buf = bytes_in;
    st.pos = 0;
    run_load(&u)
}

// ===================================================================== module init

static INIT: std::sync::Once = std::sync::Once::new();

fn make_exc(name: &str, base: &PyObject, d: &PyObject) -> PyResult {
    let ns = dict::new_dict();
    dict::set_item_str(&ns, "__module__", &pystr::new_str("_pickle"))?;
    let _ = d;
    ab::call(type_obj(type_type()), &[&pystr::new_str(name), &tuple::new_tuple(vec![base.to_ref()]), &ns])
}

pub fn init_module() -> PyResult {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        res = (|| -> PyResult<()> {
            let t = &mut *picklebuffer_type();
            t.tp_name = c"pickle.PickleBuffer".as_ptr();
            t.tp_basicsize = std::mem::size_of::<PickleBufferObj>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
            t.tp_weaklistoffset = std::mem::offset_of!(PickleBufferObj, weakreflist) as isize;
            t.tp_new = Some(pb_new);
            t.tp_dealloc = Some(pb_dealloc);
            t.tp_traverse = Some(pb_traverse);
            t.tp_clear = Some(pb_clear);
            t.tp_alloc = Some(generic_alloc);
            t.tp_getattro = Some(object_generic_getattro);
            let bp = Box::leak(Box::new(PyBufferProcs { bf_getbuffer: Some(pb_getbuffer), bf_releasebuffer: None }));
            t.tp_as_buffer = bp;
            t.tp_doc = c"PickleBuffer(buffer)\n--\n\nMarks a buffer for out-of-band transfer by protocol 5 pickling.".as_ptr();
            t.tp_methods = mdefs![
                ("raw", NOARGS, pb_raw, "raw($self, /)\n--\n\nThe wrapped memory as a byte-format memoryview (BufferError if it is not contiguous)."),
                ("release", NOARGS, pb_release, "release($self, /)\n--\n\nGive up the wrapped buffer."),
            ];
            type_ready(picklebuffer_type())?;

            for (tp, nm, copy, clear, reduce) in [
                (pmemo_type(), c"_pickle.PicklerMemoProxy", pmemo_copy as fn(&PyObject) -> PyResult, pmemo_clear as fn(&PyObject) -> PyResult, pmemo_reduce as fn(&PyObject) -> PyResult),
                (umemo_type(), c"_pickle.UnpicklerMemoProxy", umemo_copy, umemo_clear, umemo_reduce),
            ] {
                let t = &mut *tp;
                t.tp_name = nm.as_ptr();
                t.tp_basicsize = std::mem::size_of::<MemoProxyObj>() as isize;
                t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE;
                t.tp_dealloc = Some(proxy_dealloc);
                t.tp_getattro = Some(object_generic_getattro);
                let _ = (copy, clear, reduce);
                if tp == pmemo_type() {
                    t.tp_methods = mdefs![
                        ("copy", NOARGS, pmemo_copy, "The memo as a new dict."),
                        ("clear", NOARGS, pmemo_clear, "Forget every memoized object."),
                        ("__reduce__", NOARGS, pmemo_reduce, "Pickling support."),
                    ];
                } else {
                    t.tp_methods = mdefs![
                        ("copy", NOARGS, umemo_copy, "The memo as a new dict."),
                        ("clear", NOARGS, umemo_clear, "Forget every memoized object."),
                        ("__reduce__", NOARGS, umemo_reduce, "Pickling support."),
                    ];
                }
                type_ready(tp)?;
            }

            let t = &mut *pickler_type();
            t.tp_name = c"_pickle.Pickler".as_ptr();
            t.tp_basicsize = std::mem::size_of::<PicklerObj>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_IMMUTABLETYPE;
            t.tp_dictoffset = std::mem::offset_of!(PicklerObj, dict) as isize;
            t.tp_weaklistoffset = std::mem::offset_of!(PicklerObj, weakreflist) as isize;
            t.tp_new = Some(pickler_new);
            t.tp_init = Some(pickler_init);
            t.tp_dealloc = Some(pickler_dealloc);
            t.tp_alloc = Some(generic_alloc);
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_setattro = Some(object_generic_setattro);
            t.tp_doc = c"Pickler(file, protocol=None, fix_imports=True, buffer_callback=None)\n--\n\nSerialises objects to a binary file (anything with write()).".as_ptr();
            t.tp_methods = mdefs![
                ("dump", O, pickler_dump, "dump($self, obj, /)\n--\n\nPickle obj to the file."),
                ("clear_memo", NOARGS, pickler_clear_memo, "clear_memo($self, /)\n--\n\nForget the objects pickled so far (later references are pickled again)."),
                ("persistent_id", O, pickler_persistent_id, "persistent_id($self, obj, /)\n--\n\nHook for out-of-stream references: an id for obj, or None to pickle it normally."),
                ("__sizeof__", NOARGS, pickler_sizeof, "Memory used by the pickler, in bytes."),
            ];
            t.tp_getset = crate::getset_table![
                ("memo", get_pmemo, set_pmemo),
                ("bin", get_bin, set_bin),
                ("fast", get_fast, set_fast),
            ];
            type_ready(pickler_type())?;
            if let Some(d) = dict::get_item_rs(type_dict(pickler_type()), "persistent_id") {
                if d.is_type(&raw mut crate::types::descr::PyMethodDescr_Type) {
                    DEFAULT_PERSISTENT_ID = (*(d.as_ptr() as *const PyMethodDescrObject)).d_method;
                }
            }

            let t = &mut *unpickler_type();
            t.tp_name = c"_pickle.Unpickler".as_ptr();
            t.tp_basicsize = std::mem::size_of::<UnpicklerObj>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_IMMUTABLETYPE;
            t.tp_dictoffset = std::mem::offset_of!(UnpicklerObj, dict) as isize;
            t.tp_weaklistoffset = std::mem::offset_of!(UnpicklerObj, weakreflist) as isize;
            t.tp_new = Some(unpickler_new);
            t.tp_init = Some(unpickler_init);
            t.tp_dealloc = Some(unpickler_dealloc);
            t.tp_alloc = Some(generic_alloc);
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_setattro = Some(object_generic_setattro);
            t.tp_doc = c"Unpickler(file, *, fix_imports=True, encoding='ASCII', errors='strict', buffers=())\n--\n\nRebuilds objects from a pickle in a binary file (anything with read() and readline()).".as_ptr();
            t.tp_methods = mdefs![
                ("load", NOARGS, unpickler_load, "load($self, /)\n--\n\nRead one pickled object from the file."),
                ("find_class", FAST, unpickler_find_class, "find_class($self, module_name, global_name, /)\n--\n\nLook up global_name in module module_name (the hook for restricting what may be loaded)."),
                ("persistent_load", O, unpickler_persistent_load, "persistent_load($self, pid, /)\n--\n\nResolve a persistent id produced by Pickler.persistent_id()."),
                ("__sizeof__", NOARGS, unpickler_sizeof, "Memory used by the unpickler, in bytes."),
            ];
            t.tp_getset = crate::getset_table![("memo", get_umemo, set_umemo)];
            type_ready(unpickler_type())?;
            if let Some(d) = dict::get_item_rs(type_dict(unpickler_type()), "persistent_load") {
                if d.is_type(&raw mut crate::types::descr::PyMethodDescr_Type) {
                    DEFAULT_PERSISTENT_LOAD = (*(d.as_ptr() as *const PyMethodDescrObject)).d_method;
                }
            }

            let dummy = dict::new_dict();
            let pe = make_exc("PickleError", type_obj(crate::types::exceptions::Exception()), &dummy)?;
            let pk = make_exc("PicklingError", &pe, &dummy)?;
            let up = make_exc("UnpicklingError", &pe, &dummy)?;
            GLOBALS = Some(Globals { pickle_error: pe, pickling_error: pk, unpickling_error: up });
            Ok(())
        })();
    });
    res?;
    let g = globals();
    let m = crate::types::module::new_module("_pickle")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str("Native accelerator behind the pickle module."))?;
    dict::set_item_str(&d, "PickleError", &g.pickle_error)?;
    dict::set_item_str(&d, "PicklingError", &g.pickling_error)?;
    dict::set_item_str(&d, "UnpicklingError", &g.unpickling_error)?;
    dict::set_item_str(&d, "Pickler", &type_ref(pickler_type()))?;
    dict::set_item_str(&d, "Unpickler", &type_ref(unpickler_type()))?;
    dict::set_item_str(&d, "PickleBuffer", &type_ref(picklebuffer_type()))?;
    crate::builtins::add_methods_to_dict(
        &d,
        mdefs![
            ("dump", FASTKW, m_dump, "dump($module, /, obj, file, protocol=None, *, fix_imports=True,\n     buffer_callback=None)\n--\n\nPickle obj to the binary file object file."),
            ("dumps", FASTKW, m_dumps, "dumps($module, /, obj, protocol=None, *, fix_imports=True,\n      buffer_callback=None)\n--\n\nPickle obj and return the bytes."),
            ("load", FASTKW, m_load, "load($module, /, file, *, fix_imports=True, encoding='ASCII',\n     errors='strict', buffers=())\n--\n\nUnpickle one object from the binary file object file."),
            ("loads", FASTKW, m_loads, "loads($module, data, /, *, fix_imports=True, encoding='ASCII',\n      errors='strict', buffers=())\n--\n\nUnpickle one object from bytes-like data."),
        ],
        Some(&m),
    )?;
    Ok(m)
}

fn is_iterator(o: &PyObject) -> bool {
    o.ty().tp_iternext.is_some()
}
