//! TextIOWrapper and IncrementalNewlineDecoder.
use super::stringio::{SEEN_CR, SEEN_CRLF, SEEN_LF, newlines_obj};
use super::*;
use crate::abstract_::NbSlot;
use crate::modules::bufview::BufView;
use crate::modules::codecs_core;
use crate::types::bytes;
use std::ffi::{c_int, c_void};

pub static mut TEXTIOWRAPPER_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut NLDECODER_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub fn textiowrapper_type() -> *mut PyTypeObject {
    &raw mut TEXTIOWRAPPER_TYPE
}
pub fn nldecoder_type() -> *mut PyTypeObject {
    &raw mut NLDECODER_TYPE
}

const CR: u32 = '\r' as u32;
const LF: u32 = '\n' as u32;

fn cps_of(s: &PyObject) -> Vec<u32> {
    pystr::str_data(s).iter().collect()
}

// ---------------------------------------------------------------- codecs

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Builtin {
    Utf8,
    Latin1,
    Ascii,
}

fn builtin_codec(name: &str) -> Option<Builtin> {
    let n: String = name.trim().to_ascii_lowercase().chars().map(|c| if c == '-' || c == ' ' { '_' } else { c }).collect();
    match n.as_str() {
        "utf_8" | "utf8" | "u8" | "utf" | "cp65001" => Some(Builtin::Utf8),
        "latin_1" | "latin1" | "iso_8859_1" | "iso8859_1" | "l1" | "latin" | "8859" | "cp819" | "iso_ir_100" => Some(Builtin::Latin1),
        "ascii" | "us_ascii" | "646" | "us" => Some(Builtin::Ascii),
        _ => None,
    }
}

fn builtin_decode_errors_ok(e: &str) -> bool {
    matches!(e, "strict" | "ignore" | "replace" | "surrogateescape" | "backslashreplace")
}
fn builtin_encode_errors_ok(_b: Builtin, e: &str) -> bool {
    matches!(e, "strict" | "ignore" | "replace" | "surrogateescape" | "surrogatepass" | "backslashreplace" | "xmlcharrefreplace")
}

/// Encode with one of the standard error handlers.
pub fn builtin_encode(kind: Builtin, errors: &str, s: &PyObject) -> PyResult<Vec<u8>> {
    let limit: u32 = match kind {
        Builtin::Utf8 => 0x110000,
        Builtin::Latin1 => 0x100,
        Builtin::Ascii => 0x80,
    };
    let data = pystr::str_data(s);
    let n = data.len();
    let mut out = Vec::with_capacity(n);
    let ok = |c: u32| c < limit && !(kind == Builtin::Utf8 && (0xd800..0xe000).contains(&c));
    let mut i = 0;
    while i < n {
        let c = data.get(i);
        if ok(c) {
            if kind == Builtin::Utf8 {
                let mut b = [0u8; 4];
                out.extend_from_slice(char::from_u32(c).unwrap().encode_utf8(&mut b).as_bytes());
            } else {
                out.push(c as u8);
            }
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < n && !ok(data.get(j)) {
            j += 1;
        }
        for k in i..j {
            let c = data.get(k);
            match errors {
                "ignore" => {}
                "replace" => out.push(b'?'),
                "surrogateescape" if (0xdc80..0xdd00).contains(&c) => out.push((c - 0xdc00) as u8),
                "surrogatepass" if kind == Builtin::Utf8 && (0xd800..0xe000).contains(&c) => {
                    out.extend_from_slice(&[(0xe0 | (c >> 12)) as u8, (0x80 | ((c >> 6) & 0x3f)) as u8, (0x80 | (c & 0x3f)) as u8])
                }
                "backslashreplace" => {
                    let t = if c < 0x100 { format!("\\x{:02x}", c) } else if c < 0x10000 { format!("\\u{:04x}", c) } else { format!("\\U{:08x}", c) };
                    out.extend_from_slice(t.as_bytes());
                }
                "xmlcharrefreplace" => out.extend_from_slice(format!("&#{};", c).as_bytes()),
                _ => {
                    let reason = match kind {
                        Builtin::Utf8 => "surrogates not allowed".to_string(),
                        Builtin::Latin1 => "ordinal not in range(256)".to_string(),
                        Builtin::Ascii => "ordinal not in range(128)".to_string(),
                    };
                    return Err(crate::types::exceptions::unicode_encode_error(codec_name(kind), s, i, j, &reason));
                }
            }
        }
        i = j;
    }
    Ok(out)
}

fn codec_name(b: Builtin) -> &'static str {
    match b {
        Builtin::Utf8 => "utf-8",
        Builtin::Latin1 => "latin-1",
        Builtin::Ascii => "ascii",
    }
}

/// Number of trailing bytes forming an incomplete (but so far valid) UTF-8 sequence.
fn utf8_incomplete_tail(b: &[u8]) -> usize {
    let n = b.len();
    for back in 1..=3.min(n) {
        let c = b[n - back];
        if c & 0xC0 == 0x80 {
            continue;
        }
        let need = if c >= 0xF0 && c <= 0xF4 {
            4
        } else if c >= 0xE0 {
            3
        } else if c >= 0xC2 && c < 0xE0 {
            2
        } else {
            return 0;
        };
        if need > back {
            // verify the second byte restrictions to avoid holding obviously invalid data
            if back >= 2 {
                let c2 = b[n - back + 1];
                let ok = match c {
                    0xE0 => (0xA0..=0xBF).contains(&c2),
                    0xED => (0x80..=0x9F).contains(&c2),
                    0xF0 => (0x90..=0xBF).contains(&c2),
                    0xF4 => (0x80..=0x8F).contains(&c2),
                    _ => true,
                };
                if !ok {
                    return 0;
                }
            }
            return back;
        }
        return 0;
    }
    0
}

pub enum Inner {
    Builtin { kind: Builtin, errors: String, pending: Vec<u8> },
    Py(PyRef),
}

impl Inner {
    fn decode(&mut self, input: &[u8], fin: bool) -> PyResult<Vec<u32>> {
        match self {
            Inner::Builtin { kind, errors, pending } => {
                let data: Vec<u8>;
                let mut src: &[u8] = input;
                if !pending.is_empty() {
                    let mut v = std::mem::take(pending);
                    v.extend_from_slice(input);
                    data = v;
                    src = &data;
                }
                let s = match kind {
                    Builtin::Utf8 => {
                        let keep = if fin { 0 } else { utf8_incomplete_tail(src) };
                        let (head, tail) = src.split_at(src.len() - keep);
                        let r = codecs_core::decode_utf8(head, errors)?;
                        *pending = tail.to_vec();
                        r
                    }
                    Builtin::Latin1 => pystr::from_latin1(src),
                    Builtin::Ascii => codecs_core::decode_bytes(src, "ascii", errors)?,
                };
                Ok(cps_of(&s))
            }
            Inner::Py(d) => {
                let b = new_bytes(input);
                let r = ab::call_method_str(d, "decode", &[&b, &bool_::new_bool(fin)])?;
                if !pystr::is_str(&r) {
                    raise!(TypeError, "decoder should return a string result, not '{}'", tname(&r));
                }
                Ok(cps_of(&r))
            }
        }
    }
    fn getstate(&self) -> PyResult<(Vec<u8>, i64)> {
        match self {
            Inner::Builtin { pending, .. } => Ok((pending.clone(), 0)),
            Inner::Py(d) => {
                let st = call0(d, "getstate")?;
                if !tuple::is_tuple(&st) || tuple::tuple_len(&st) != 2 {
                    raise!(TypeError, "illegal decoder state");
                }
                let b = tuple::tuple_get(&st, 0);
                if !bytes::is_bytes(b) {
                    raise!(TypeError, "illegal decoder state: the first item must be bytes, not '{}'", tname(b));
                }
                let flags = ab::as_i64(tuple::tuple_get(&st, 1))?;
                Ok((bytes::bytes_data(b).to_vec(), flags))
            }
        }
    }
    fn setstate(&mut self, buf: &[u8], flags: i64) -> PyResult<()> {
        match self {
            Inner::Builtin { pending, .. } => {
                *pending = buf.to_vec();
                Ok(())
            }
            Inner::Py(d) => {
                let st = tuple::new_tuple(vec![new_bytes(buf), int::new_int(flags)]);
                call1(d, "setstate", &st)?;
                Ok(())
            }
        }
    }
    fn reset(&mut self) -> PyResult<()> {
        match self {
            Inner::Builtin { pending, .. } => {
                pending.clear();
                Ok(())
            }
            Inner::Py(d) => call0(d, "reset").map(|_| ()),
        }
    }
}

#[derive(Default, Clone, Copy)]
pub struct Nl {
    pub translate: bool,
    pub pendingcr: bool,
    pub seennl: u8,
}

impl Nl {
    pub fn process(&mut self, mut out: Vec<u32>, fin: bool) -> Vec<u32> {
        if self.pendingcr && (!out.is_empty() || fin) {
            out.insert(0, CR);
            self.pendingcr = false;
        }
        if !fin && out.last() == Some(&CR) {
            out.pop();
            self.pendingcr = true;
        }
        let mut i = 0;
        let mut seen = 0u8;
        while i < out.len() {
            if out[i] == CR {
                if i + 1 < out.len() && out[i + 1] == LF {
                    seen |= SEEN_CRLF;
                    i += 2;
                    continue;
                }
                seen |= SEEN_CR;
            } else if out[i] == LF {
                seen |= SEEN_LF;
            }
            i += 1;
        }
        self.seennl |= seen;
        if self.translate && seen & (SEEN_CR | SEEN_CRLF) != 0 {
            let mut t = Vec::with_capacity(out.len());
            let mut i = 0;
            while i < out.len() {
                if out[i] == CR {
                    t.push(LF);
                    if i + 1 < out.len() && out[i + 1] == LF {
                        i += 1;
                    }
                } else {
                    t.push(out[i]);
                }
                i += 1;
            }
            return t;
        }
        out
    }
}

pub struct Decoder {
    pub inner: Inner,
    pub nl: Option<Nl>,
}

impl Decoder {
    fn decode(&mut self, input: &[u8], fin: bool) -> PyResult<Vec<u32>> {
        let out = self.inner.decode(input, fin)?;
        Ok(match &mut self.nl {
            Some(nl) => nl.process(out, fin),
            None => out,
        })
    }
    fn getstate(&self) -> PyResult<(Vec<u8>, i64)> {
        let (b, f) = self.inner.getstate()?;
        Ok(match &self.nl {
            Some(nl) => (b, (f << 1) | nl.pendingcr as i64),
            None => (b, f),
        })
    }
    fn setstate(&mut self, buf: &[u8], flags: i64) -> PyResult<()> {
        match &mut self.nl {
            Some(nl) => {
                nl.pendingcr = flags & 1 != 0;
                self.inner.setstate(buf, flags >> 1)
            }
            None => self.inner.setstate(buf, flags),
        }
    }
    fn reset(&mut self) -> PyResult<()> {
        if let Some(nl) = &mut self.nl {
            nl.pendingcr = false;
            nl.seennl = 0;
        }
        self.inner.reset()
    }
}

pub enum Encoder {
    Builtin { kind: Builtin, errors: PyRef },
    Py(PyRef),
}

impl Encoder {
    fn encode(&self, s: &PyObject) -> PyResult<PyRef> {
        match self {
            Encoder::Builtin { kind, errors } => {
                Ok(new_bytes(&builtin_encode(*kind, &pystr::to_string_lossy(errors), s)?))
            }
            Encoder::Py(e) => {
                let r = call1(e, "encode", s)?;
                if !bytes::is_bytes(&r) {
                    raise!(TypeError, "encoder should return a bytes object, not '{}'", tname(&r));
                }
                Ok(r)
            }
        }
    }
    fn reset(&self, start_of_stream: bool) -> PyResult<()> {
        if let Encoder::Py(e) = self {
            if start_of_stream {
                call0(e, "reset")?;
            } else {
                call1(e, "setstate", &int::new_int(0))?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- TextIOWrapper state

pub struct State {
    pub buffer: PyRef,
    pub encoding: PyRef,
    pub errors: PyRef,
    pub newline: Option<String>,
    pub decoder: Option<Decoder>,
    pub encoder: Option<Encoder>,
    pub readuniversal: bool,
    pub readtranslate: bool,
    pub writetranslate: bool,
    pub readnl: Option<Vec<u32>>,
    pub writenl: Option<String>,
    pub line_buffering: bool,
    pub write_through: bool,
    pub decoded: Vec<u32>,
    pub used: usize,
    pub pending: Vec<u8>,
    pub snapshot: Option<(i64, Vec<u8>)>,
    pub seekable: bool,
    pub telling: bool,
    pub has_read1: bool,
    pub b2cratio: f64,
    pub chunk_size: usize,
    pub py_decoder_obj: Option<PyRef>,
}

#[repr(C)]
pub struct TextIO {
    pub hdr: IoHeader,
    pub st: *mut State,
    pub detached: bool,
    pub finalizing: bool,
}

#[inline]
fn tio(o: &PyObject) -> &mut TextIO {
    unsafe { &mut *(o.as_ptr() as *mut TextIO) }
}

fn state(o: &PyObject) -> PyResult<&mut State> {
    if tio(o).detached {
        raise!(ValueError, "underlying buffer has been detached");
    }
    state_any(o)
}

/// The state even after detach() (which keeps encoding, errors, ... and only
/// drops the buffer); used by attributes that outlive the buffer and by an
/// operation that was already running when the buffer got detached.
fn state_any(o: &PyObject) -> PyResult<&mut State> {
    let t = tio(o);
    if t.st.is_null() {
        raise!(ValueError, "I/O operation on uninitialized object");
    }
    Ok(unsafe { &mut *t.st })
}

fn check_open(o: &PyObject) -> PyResult<&mut State> {
    let st = state(o)?;
    let closed = if let Some(c) = super::bufferedio::fast_closed(&st.buffer) {
        c
    } else {
        let c = ab::getattr(&st.buffer, crate::name!("closed"))?;
        ab::is_true(&c)?
    };
    if closed {
        raise!(ValueError, "I/O operation on closed file.");
    }
    state(o)
}

unsafe extern "C" fn textio_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe { (*tp).tp_alloc.unwrap()(tp, 0) }
}

fn validate_newline(nl: Option<&PyObject>) -> PyResult<Option<String>> {
    match nl {
        None => Ok(None),
        Some(x) if x.is(none::none()) => Ok(None),
        Some(x) => {
            if !pystr::is_str(x) {
                raise!(TypeError, "TextIOWrapper() argument 'newline' must be str or None, not {}", tname(x));
            }
            let s = pystr::to_string_lossy(x);
            if s.contains('\0') {
                raise!(ValueError, "embedded null character");
            }
            if !matches!(s.as_str(), "" | "\n" | "\r" | "\r\n") {
                raise!(ValueError, "illegal newline value: {}", ab::repr_string(x)?);
            }
            Ok(Some(s))
        }
    }
}

/// Resolve the encoding argument to the str stored in `encoding`.
fn resolve_encoding(enc: Option<&PyObject>) -> PyResult<PyRef> {
    let e = match enc {
        None => none::none_ref(),
        Some(x) => x.to_ref(),
    };
    if e.is(none::none()) {
        let flags = crate::modules::sys::get_sys_attr("flags");
        let warn = flags
            .as_ref()
            .and_then(|f| ab::getattr_str(f, "warn_default_encoding").ok())
            .and_then(|v| int::as_i64(&v))
            .unwrap_or(0);
        if warn != 0 {
            crate::modules::warnings::warn(crate::types::exceptions::EncodingWarning(), "'encoding' argument not specified", 1)?;
        }
        clear_exc_if_attr();
        return Ok(pystr::new_str("utf-8"));
    }
    if !pystr::is_str(&e) {
        raise!(TypeError, "TextIOWrapper() argument 'encoding' must be str or None, not {}", tname(&e));
    }
    // like the C API, the name must be encodable (surrogates raise UnicodeEncodeError)
    let s = pystr::as_utf8(&e)?.to_string();
    if s.contains('\0') {
        raise!(ValueError, "embedded null character");
    }
    if s == "locale" {
        return Ok(pystr::new_str("utf-8"));
    }
    Ok(e)
}

fn clear_exc_if_attr() {
    if exc_matches(crate::types::exceptions::AttributeError()) {
        clear_exc();
    }
}

/// codecs.lookup(encoding) with the text-encoding check.
fn lookup_codec(enc: &str) -> PyResult<PyRef> {
    let codecs = crate::import::import_absolute("codecs")?;
    let info = ab::call_method_str(&codecs, "lookup", &[&pystr::new_str(enc)])?;
    if let Some(is_text) = ab::getattr_opt(&info, &pystr::intern_str("_is_text_encoding"))? {
        if !ab::is_true(&is_text)? {
            raise!(LookupError, "'{}' is not a text encoding; use codecs.open() to handle arbitrary codecs", enc);
        }
    }
    Ok(info)
}

fn build_codecs(st: &mut State, readable: bool, writable: bool) -> PyResult<()> {
    let enc = pystr::to_string_lossy(&st.encoding);
    let errs = pystr::to_string_lossy(&st.errors);
    let builtin = builtin_codec(&enc);
    let mut info: Option<PyRef> = None;
    if builtin.is_none() {
        info = Some(lookup_codec(&enc)?);
    }
    st.decoder = None;
    st.encoder = None;
    st.py_decoder_obj = None;
    if readable {
        let inner = match builtin {
            Some(b) if builtin_decode_errors_ok(&errs) => Inner::Builtin { kind: b, errors: errs.clone(), pending: Vec::new() },
            _ => {
                let info = match &info {
                    Some(i) => i.clone(),
                    None => lookup_codec(&enc)?,
                };
                let f = ab::getattr_str(&info, "incrementaldecoder")?;
                if f.is(none::none()) {
                    raise!(TypeError, "codec has no incremental decoder");
                }
                let d = ab::call(&f, &[&st.errors])?;
                st.py_decoder_obj = Some(d.clone());
                Inner::Py(d)
            }
        };
        let nl = if st.readuniversal { Some(Nl { translate: st.readtranslate, ..Default::default() }) } else { None };
        st.decoder = Some(Decoder { inner, nl });
    }
    if writable {
        let e = match builtin {
            Some(b) if builtin_encode_errors_ok(b, &errs) => Encoder::Builtin { kind: b, errors: st.errors.clone() },
            _ => {
                let info = match &info {
                    Some(i) => i.clone(),
                    None => lookup_codec(&enc)?,
                };
                let f = ab::getattr_str(&info, "incrementalencoder")?;
                if f.is(none::none()) {
                    raise!(TypeError, "codec has no incremental encoder");
                }
                Encoder::Py(ab::call(&f, &[&st.errors])?)
            }
        };
        st.encoder = Some(e);
    }
    Ok(())
}

fn configure_newline(st: &mut State, newline: Option<String>) {
    st.readuniversal = newline.as_deref().map_or(true, |n| n.is_empty());
    st.readtranslate = newline.is_none();
    st.writetranslate = newline.as_deref().map_or(true, |n| !n.is_empty());
    st.readnl = newline.as_ref().map(|n| n.chars().map(|c| c as u32).collect());
    st.writenl = match newline.as_deref() {
        Some("\r") | Some("\r\n") => newline.clone(),
        _ => None,
    };
    st.newline = newline;
}

fn textio_init_impl(
    o: &PyObject,
    buffer: &PyObject,
    encoding: Option<&PyObject>,
    errors: Option<&PyObject>,
    newline: Option<&PyObject>,
    line_buffering: bool,
    write_through: bool,
) -> PyResult<()> {
    let t = tio(o);
    if !t.st.is_null() {
        unsafe { drop(Box::from_raw(t.st)) };
        t.st = std::ptr::null_mut();
    }
    t.detached = false;
    let enc = resolve_encoding(encoding)?;
    let errors = match errors {
        None => pystr::new_str("strict"),
        Some(e) if e.is(none::none()) => pystr::new_str("strict"),
        Some(e) => {
            if !pystr::is_str(e) {
                raise!(TypeError, "TextIOWrapper() argument 'errors' must be str or None, not {}", tname(e));
            }
            if pystr::as_utf8(e)?.contains('\0') {
                raise!(ValueError, "embedded null character");
            }
            // development mode validates the handler name up front
            if crate::modules::sys_full::config().dev_mode {
                let codecs = crate::import::import_absolute("codecs")?;
                let lookup: PyRef = ab::getattr_str(&codecs, "lookup_error")?;
                ab::call(&lookup, &[e])?;
            }
            e.to_ref()
        }
    };
    let newline = validate_newline(newline)?;
    let mut st = State {
        buffer: buffer.to_ref(),
        encoding: enc,
        errors,
        newline: None,
        decoder: None,
        encoder: None,
        readuniversal: true,
        readtranslate: true,
        writetranslate: true,
        readnl: None,
        writenl: None,
        line_buffering,
        write_through,
        decoded: Vec::new(),
        used: 0,
        pending: Vec::new(),
        snapshot: None,
        seekable: false,
        telling: false,
        has_read1: false,
        b2cratio: 0.0,
        chunk_size: 8192,
        py_decoder_obj: None,
    };
    configure_newline(&mut st, newline);
    let readable = ab::is_true(&*call0(buffer, "readable")?)?;
    let writable = ab::is_true(&*call0(buffer, "writable")?)?;
    build_codecs(&mut st, readable, writable)?;
    st.seekable = ab::is_true(&*call0(buffer, "seekable")?)?;
    st.telling = st.seekable;
    st.has_read1 = ab::hasattr(buffer, &pystr::intern_str("read1"))?;
    if st.seekable && writable {
        if let Some(e) = &st.encoder {
            let pos = call0(buffer, "tell")?;
            if !ab::rich_eq(&pos, &int::new_int(0))? {
                e.reset(false)?;
            }
        }
    }
    tio(o).st = Box::into_raw(Box::new(st));
    Ok(())
}

unsafe extern "C" fn textio_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let o = &*op;
        let r = (|| -> PyResult<()> {
            let (vals, kws) = init_args(args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            let [buffer, encoding, errors, newline, lb, wt] = kwparse::<6>(
                "TextIOWrapper",
                &["buffer", "encoding", "errors", "newline", "line_buffering", "write_through"],
                0,
                1,
                &refs,
                kws.as_deref(),
            )?;
            let lb = match lb {
                Some(x) => ab::is_true(x)?,
                None => false,
            };
            let wt = match wt {
                Some(x) => ab::is_true(x)?,
                None => false,
            };
            textio_init_impl(o, buffer.unwrap(), encoding, errors, newline, lb, wt)
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn textio_dealloc(op: *mut PyObject) {
    unsafe {
        if dealloc_begin(op) {
            return;
        }
        let t = &mut *(op as *mut TextIO);
        if !t.st.is_null() {
            drop(Box::from_raw(t.st));
            t.st = std::ptr::null_mut();
        }
        dealloc_end(op);
    }
}
unsafe extern "C" fn textio_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let r = visit_header(op, visit, arg);
        if r != 0 {
            return r;
        }
        let t = &*(op as *mut TextIO);
        if !t.st.is_null() {
            let st = &*t.st;
            let r = visit.unwrap()(st.buffer.as_ptr(), arg);
            if r != 0 {
                return r;
            }
            if let Some(d) = &st.py_decoder_obj {
                let r = visit.unwrap()(d.as_ptr(), arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}
unsafe extern "C" fn textio_clear(op: *mut PyObject) -> c_int {
    unsafe {
        clear_slot(&mut hdr(&*op).dict);
        let t = &mut *(op as *mut TextIO);
        if !t.st.is_null() {
            drop(Box::from_raw(t.st));
            t.st = std::ptr::null_mut();
        }
        0
    }
}

// ---------------------------------------------------------------- writing

fn flush_pending(st: &mut State) -> PyResult<()> {
    if st.pending.is_empty() {
        return Ok(());
    }
    let b = new_bytes(&st.pending);
    st.pending.clear();
    loop {
        match call1(&st.buffer, "write", &b) {
            Ok(_) => return Ok(()),
            Err(e) => {
                if exc_matches(crate::types::exceptions::InterruptedError()) {
                    clear_exc();
                    continue;
                }
                return Err(e);
            }
        }
    }
}

fn t_write(o: &PyObject, text: &PyObject) -> PyResult {
    let st = check_open(o)?;
    if !pystr::is_str(text) {
        raise!(TypeError, "write() argument must be str, not {}", tname(text));
    }
    if st.encoder.is_none() {
        return Err(unsupported("not writable"));
    }
    let n = pystr::str_len(text);
    let data = pystr::str_data(text);
    let has_lf = (st.writetranslate || st.line_buffering) && data.iter().any(|c| c == LF);
    let mut text_obj = text.to_ref();
    if has_lf && st.writetranslate {
        if let Some(nl) = &st.writenl {
            let mut out: Vec<u32> = Vec::with_capacity(n + 8);
            for c in data.iter() {
                if c == LF {
                    out.extend(nl.chars().map(|c| c as u32));
                } else {
                    out.push(c);
                }
            }
            text_obj = pystr::from_codepoints(&out);
        }
    }
    let needflush = st.line_buffering && (has_lf || pystr::str_data(text).iter().any(|c| c == CR));
    let b = st.encoder.as_ref().unwrap().encode(&text_obj)?;
    let data = bytes::bytes_data(&b);
    // Pending bytes that would overflow the chunk are written out first
    // (repeatedly: a write() of the buffer may re-enter and add more).
    loop {
        let st = state(o)?;
        if st.pending.is_empty() || st.pending.len() + data.len() <= st.chunk_size {
            break;
        }
        flush_pending(st)?;
    }
    let st = state(o)?;
    st.pending.extend_from_slice(data);
    if st.pending.len() >= st.chunk_size || needflush || st.write_through {
        flush_pending(st)?;
    }
    if needflush {
        call0(&state(o)?.buffer, "flush")?;
    }
    let st = state(o)?;
    // read-ahead is stale once the position moved; a non-seekable stream
    // (e.g. BufferedRWPair) reads and writes independently and keeps it
    if st.seekable {
        st.decoded.clear();
        st.used = 0;
        st.snapshot = None;
        if let Some(d) = &mut st.decoder {
            d.reset()?;
        }
    }
    Ok(int::new_int(n as i64))
}

fn t_flush(o: &PyObject) -> PyResult {
    let st = check_open(o)?;
    st.telling = st.seekable;
    flush_pending(st)?;
    call0(&state(o)?.buffer, "flush")
}

// ---------------------------------------------------------------- reading

/// Read and decode one chunk. Returns false at EOF.
fn read_chunk(st: &mut State, size_hint: usize) -> PyResult<bool> {
    if st.decoder.is_none() {
        return Err(unsupported("not readable"));
    }
    let (dec_buffer, dec_flags) = if st.telling { st.decoder.as_ref().unwrap().getstate()? } else { (Vec::new(), 0) };
    let mut size = st.chunk_size;
    if size_hint > 0 {
        let est = (size_hint as f64 * st.b2cratio.max(1.0)) as usize;
        size = size.max(est);
    }
    let method = if st.has_read1 { "read1" } else { "read" };
    let buf = st.buffer.clone();
    let chunk = ab::call_method_str(&buf, method, &[&int::new_int(size as i64)])?;
    if !BufView::has_buffer(&chunk) {
        raise!(TypeError, "underlying {}() should have returned a bytes-like object, not '{}'", method, tname(&chunk));
    }
    let view = BufView::get(&chunk, false)?;
    let input = view.as_slice();
    let eof = input.is_empty();
    let decoded = st.decoder.as_mut().unwrap().decode(input, eof)?;
    st.b2cratio = if decoded.is_empty() { 0.0 } else { input.len() as f64 / decoded.len() as f64 };
    st.decoded = decoded;
    st.used = 0;
    if st.telling {
        let mut next = dec_buffer;
        next.extend_from_slice(input);
        st.snapshot = Some((dec_flags, next));
    }
    Ok(!eof)
}

fn take_decoded(st: &mut State, n: usize) -> &[u32] {
    let avail = st.decoded.len() - st.used;
    let k = n.min(avail);
    let s = &st.decoded[st.used..st.used + k];
    st.used += k;
    s
}

fn t_read(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("read", args, 0, 1)?;
    let n = size_arg(args.first().copied())?;
    let st = check_open(o)?;
    if st.decoder.is_none() {
        return Err(unsupported("not readable"));
    }
    flush_pending(st)?;
    let st = state(o)?;
    if n < 0 {
        let buf = st.buffer.clone();
        let data = call0(&buf, "read")?;
        if data.is(none::none()) {
            raise!(BlockingIOError, "Read returned None.");
        }
        if !BufView::has_buffer(&data) {
            raise!(TypeError, "underlying read() should have returned a bytes object, not '{}'", tname(&data));
        }
        let view = BufView::get(&data, false)?;
        let st = state_any(o)?;
        let mut out: Vec<u32> = st.decoded[st.used.min(st.decoded.len())..].to_vec();
        let more = st.decoder.as_mut().unwrap().decode(view.as_slice(), true)?;
        out.extend_from_slice(&more);
        st.decoded.clear();
        st.used = 0;
        st.snapshot = None;
        return Ok(pystr::from_codepoints(&out));
    }
    let n = n as usize;
    let mut out: Vec<u32> = take_decoded(st, n).to_vec();
    while out.len() < n {
        let st = state_any(o)?;
        if !read_chunk(st, n - out.len())? {
            let st = state_any(o)?;
            out.extend_from_slice(take_decoded(st, n - out.len()));
            break;
        }
        let st = state_any(o)?;
        let need = n - out.len();
        out.extend_from_slice(take_decoded(st, need));
    }
    Ok(pystr::from_codepoints(&out))
}

/// Find the end of a line in `chunk` (exclusive). `prev_cr`: the text before the
/// chunk ended with a pending '\r' (universal mode only).
/// Returns (Some(end), _) if found; ambiguous trailing '\r' returns (None, true).
fn find_eol(st: &State, chunk: &[u32]) -> (Option<usize>, bool) {
    if st.readtranslate || (!st.readuniversal && st.readnl.as_deref() == Some(&[LF][..])) {
        return (chunk.iter().position(|&c| c == LF).map(|i| i + 1), false);
    }
    if st.readuniversal {
        for (i, &c) in chunk.iter().enumerate() {
            if c == LF {
                return (Some(i + 1), false);
            }
            if c == CR {
                if i + 1 < chunk.len() {
                    return (Some(if chunk[i + 1] == LF { i + 2 } else { i + 1 }), false);
                }
                return (None, true);
            }
        }
        return (None, false);
    }
    let nl = st.readnl.as_deref().unwrap_or(&[LF]);
    let k = nl.len();
    if chunk.len() >= k {
        for i in 0..=chunk.len() - k {
            if chunk[i..i + k] == *nl {
                return (Some(i + k), false);
            }
        }
    }
    (None, false)
}

fn readline_impl(o: &PyObject, limit: isize) -> PyResult {
    let st = check_open(o)?;
    if st.decoder.is_none() {
        return Err(unsupported("not readable"));
    }
    flush_pending(st)?;
    let mut line: Vec<u32> = Vec::new();
    let mut pending_cr = false;
    loop {
        let st = state_any(o)?;
        let chunk = &st.decoded[st.used.min(st.decoded.len())..];
        if pending_cr {
            // previous chunk ended with '\r' in universal mode
            if !chunk.is_empty() {
                if chunk[0] == LF {
                    line.push(LF);
                    st.used += 1;
                }
                break;
            }
        } else if !chunk.is_empty() {
            let room = if limit < 0 { usize::MAX } else { (limit as usize).saturating_sub(line.len()) };
            let (end, ambiguous) = find_eol(st, chunk);
            match end {
                Some(e) if e <= room => {
                    line.extend_from_slice(&chunk[..e]);
                    st.used += e;
                    break;
                }
                _ => {
                    if chunk.len() >= room {
                        line.extend_from_slice(&chunk[..room]);
                        st.used += room;
                        break;
                    }
                    line.extend_from_slice(chunk);
                    st.used += chunk.len();
                    if ambiguous {
                        pending_cr = true;
                    }
                }
            }
        }
        if limit >= 0 && line.len() >= limit as usize {
            break;
        }
        let st = state_any(o)?;
        if !read_chunk(st, 0)? {
            let st = state_any(o)?;
            // EOF: take whatever is left (decoder may have emitted final chars)
            let rest = &st.decoded[st.used..];
            if !rest.is_empty() && !pending_cr {
                let room = if limit < 0 { usize::MAX } else { (limit as usize).saturating_sub(line.len()) };
                let (end, _) = find_eol(st, rest);
                let e = end.unwrap_or(rest.len()).min(room);
                line.extend_from_slice(&rest[..e]);
                st.used += e;
            }
            break;
        }
    }
    Ok(pystr::from_codepoints(&line))
}

fn t_readline(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("readline", args, 0, 1)?;
    let limit = match args.first() {
        None => -1,
        Some(x) if x.is(none::none()) => -1,
        Some(x) => {
            if !ab::has_index(x) {
                raise!(TypeError, "argument should be integer or None, not '{}'", tname(x));
            }
            ssize_arg(x)?
        }
    };
    readline_impl(o, limit)
}

unsafe extern "C" fn t_iternext(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let r = (|| -> PyResult<Option<PyRef>> {
            {
                let st = state(o)?;
                st.telling = false;
            }
            let line = if o.is_type(textiowrapper_type()) {
                readline_impl(o, -1)?
            } else {
                let l = call0(o, "readline")?;
                if !pystr::is_str(&l) {
                    raise!(OSError, "readline() should have returned a str object, not '{}'", tname(&l));
                }
                l
            };
            if pystr::str_len(&line) == 0 {
                let st = state(o)?;
                st.snapshot = None;
                st.telling = st.seekable;
                return Ok(None);
            }
            Ok(Some(line))
        })();
        match r {
            Ok(Some(l)) => l.into_raw(),
            _ => std::ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------- tell / seek

fn shl(v: PyRef, bits: i64) -> PyResult {
    ab::binary_op(&v, &int::new_int(bits), NbSlot::Lshift)
}

fn build_cookie(pos: i64, dec_flags: i64, bytes_to_feed: i64, need_eof: bool, chars_to_skip: i64) -> PyResult {
    if dec_flags == 0 && bytes_to_feed == 0 && !need_eof && chars_to_skip == 0 && pos >= 0 {
        return Ok(int::new_int(pos));
    }
    let mut r = int::new_int(pos);
    for (v, sh) in [(dec_flags, 64), (bytes_to_feed, 128), (chars_to_skip, 192), (need_eof as i64, 256)] {
        if v != 0 {
            let part = shl(int::new_int(v), sh)?;
            r = ab::binary_op(&r, &part, NbSlot::Or)?;
        }
    }
    Ok(r)
}

fn unpack_cookie(c: &PyObject) -> PyResult<(i64, i64, i64, bool, i64)> {
    if let Some(v) = int::as_i64(c) {
        if v >= 0 {
            return Ok((v, 0, 0, false, 0));
        }
    }
    let mask = ab::binary_op(&*shl(int::new_int(1), 64)?, &int::new_int(1), NbSlot::Sub)?;
    let mut parts = [0i64; 5];
    let mut cur = c.to_ref();
    for p in parts.iter_mut() {
        let low = ab::binary_op(&cur, &mask, NbSlot::And)?;
        *p = match int::as_i128(&low) {
            Some(v) => v as u64 as i64,
            None => raise!(OverflowError, "cookie too large"),
        };
        cur = ab::binary_op(&cur, &int::new_int(64), NbSlot::Rshift)?;
    }
    Ok((parts[0], parts[1], parts[2], parts[4] != 0, parts[3]))
}

fn t_tell(o: &PyObject) -> PyResult {
    let st = check_open(o)?;
    if !st.seekable {
        return Err(unsupported("underlying stream is not seekable"));
    }
    if !st.telling {
        raise!(OSError, "telling position disabled by next() call");
    }
    call0(o, "flush")?;
    let st = state(o)?;
    let posobj = call0(&st.buffer, "tell")?;
    let st = state(o)?;
    let (dec_flags, next_input) = match (&st.decoder, &st.snapshot) {
        (Some(_), Some(s)) => s.clone(),
        _ => return Ok(posobj),
    };
    let position = ab::as_i64(&posobj)? - next_input.len() as i64;
    let mut chars_to_skip = st.used as i64;
    if chars_to_skip == 0 {
        return build_cookie(position, dec_flags, 0, false, 0);
    }
    let dec = st.decoder.as_mut().unwrap();
    let saved = dec.getstate()?;
    let result = (|| -> PyResult {
        dec.setstate(&[], dec_flags)?;
        let mut start_pos = position;
        let mut start_flags = dec_flags;
        let mut bytes_fed = 0i64;
        let mut chars_decoded = 0i64;
        let mut need_eof = false;
        let mut done = false;
        for i in 0..next_input.len() {
            bytes_fed += 1;
            chars_decoded += dec.decode(&next_input[i..i + 1], false)?.len() as i64;
            let (buf, flags) = dec.getstate()?;
            if buf.is_empty() && chars_decoded <= chars_to_skip {
                start_pos += bytes_fed;
                chars_to_skip -= chars_decoded;
                start_flags = flags;
                bytes_fed = 0;
                chars_decoded = 0;
            }
            if chars_decoded >= chars_to_skip {
                done = true;
                break;
            }
        }
        if !done {
            chars_decoded += dec.decode(&[], true)?.len() as i64;
            need_eof = true;
            if chars_decoded < chars_to_skip {
                raise!(OSError, "can't reconstruct logical file position");
            }
        }
        build_cookie(start_pos, start_flags, bytes_fed, need_eof, chars_to_skip)
    })();
    let st = state(o)?;
    let restore = st.decoder.as_mut().unwrap().setstate(&saved.0, saved.1);
    let r = result?;
    restore?;
    Ok(r)
}

fn reset_encoder(st: &State, start: bool) -> PyResult<()> {
    if let Some(e) = &st.encoder {
        e.reset(start)?;
    }
    Ok(())
}

fn t_seek(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("seek", args, 1, 2)?;
    let mut cookie = args[0].to_ref();
    let whence = if args.len() > 1 { crate::modules::posix::path::c_int(args[1])? } else { 0 };
    let st = check_open(o)?;
    if !st.seekable {
        return Err(unsupported("underlying stream is not seekable"));
    }
    let zero = int::new_int(0);
    match whence {
        1 => {
            if !ab::rich_eq(&cookie, &zero)? {
                return Err(unsupported("can't do nonzero cur-relative seeks"));
            }
            cookie = call0(o, "tell")?;
        }
        2 => {
            if !ab::rich_eq(&cookie, &zero)? {
                return Err(unsupported("can't do nonzero end-relative seeks"));
            }
            call0(o, "flush")?;
            let st = state(o)?;
            st.decoded.clear();
            st.used = 0;
            st.snapshot = None;
            if let Some(d) = &mut st.decoder {
                d.reset()?;
            }
            let buf = st.buffer.clone();
            let res = ab::call_method_str(&buf, "seek", &[&zero, &int::new_int(2)])?;
            let st = state(o)?;
            let at_start = ab::rich_eq(&res, &zero)?;
            reset_encoder(st, at_start)?;
            return Ok(res);
        }
        0 => {}
        _ => raise!(ValueError, "invalid whence ({}, should be 0, 1 or 2)", whence),
    }
    if !int::is_int(&cookie) {
        if crate::types::float::is_float(&cookie) {
            raise!(TypeError, "an integer is required");
        }
        cookie = ab::index(&cookie)?;
    }
    if int::sign(&cookie) < 0 {
        raise!(ValueError, "negative seek position {}", ab::repr_string(&cookie)?);
    }
    call0(o, "flush")?;
    let (start_pos, dec_flags, bytes_to_feed, need_eof, chars_to_skip) = unpack_cookie(&cookie)?;
    let buf = state(o)?.buffer.clone();
    ab::call_method_str(&buf, "seek", &[&int::new_int(start_pos)])?;
    let st = state(o)?;
    st.decoded.clear();
    st.used = 0;
    st.snapshot = None;
    let is_zero = ab::rich_eq(&cookie, &zero)?;
    if let Some(d) = &mut st.decoder {
        if is_zero {
            d.reset()?;
        } else {
            d.setstate(&[], dec_flags)?;
            st.snapshot = Some((dec_flags, Vec::new()));
        }
    }
    if chars_to_skip != 0 {
        let input = ab::call_method_str(&buf, "read", &[&int::new_int(bytes_to_feed)])?;
        let view = BufView::get(&input, false)?;
        let st = state(o)?;
        let decoded = st.decoder.as_mut().unwrap().decode(view.as_slice(), need_eof)?;
        st.snapshot = Some((dec_flags, view.as_slice().to_vec()));
        if (decoded.len() as i64) < chars_to_skip {
            raise!(OSError, "can't restore logical file position");
        }
        st.decoded = decoded;
        st.used = chars_to_skip as usize;
    }
    let st = state(o)?;
    reset_encoder(st, is_zero)?;
    Ok(cookie)
}

fn t_truncate(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("truncate", args, 0, 1)?;
    call0(o, "flush")?;
    let pos = match args.first() {
        Some(p) if !p.is(none::none()) => p.to_ref(),
        _ => call0(o, "tell")?,
    };
    let buf = state(o)?.buffer.clone();
    call1(&buf, "truncate", &pos)
}

// ---------------------------------------------------------------- misc

fn t_close(o: &PyObject) -> PyResult {
    let st = state(o)?;
    let buf = st.buffer.clone();
    let c = ab::getattr(&buf, crate::name!("closed"))?;
    // the buffer's `closed` may have detached us: nothing left to close
    if ab::is_true(&c)? || tio(o).detached {
        return Ok(none::none_ref());
    }
    if tio(o).finalizing || hdr(o).ioflags & F_FINALIZING != 0 {
        if let Ok(Some(dw)) = ab::getattr_opt(&buf, &pystr::intern_str("_dealloc_warn")) {
            if ab::call(&dw, &[o]).is_err() {
                clear_exc();
            }
        } else {
            clear_exc();
        }
    }
    let flush_res = call0(o, "flush");
    let flush_exc = if flush_res.is_err() { take_exc() } else { None };
    let close_res = call0(&buf, "close");
    match (close_res, flush_exc) {
        (Ok(_), None) => Ok(none::none_ref()),
        (Ok(_), Some(fe)) => {
            set_exc(fe);
            Err(PyErr)
        }
        (Err(e), None) => Err(e),
        (Err(e), Some(fe)) => {
            if let Some(ce) = take_exc() {
                crate::types::exceptions::set_context_and_cause(&ce, Some(&fe));
                set_exc(ce);
            }
            Err(e)
        }
    }
}

fn t_detach(o: &PyObject) -> PyResult {
    state(o)?;
    call0(o, "flush")?;
    // flush() may itself have detached us
    let st = state(o)?;
    let buf = std::mem::replace(&mut st.buffer, none::none_ref());
    tio(o).detached = true;
    Ok(buf)
}

macro_rules! fwd {
    ($fname:ident, $meth:literal) => {
        fn $fname(o: &PyObject) -> PyResult {
            let buf = state(o)?.buffer.clone();
            call0(&buf, $meth)
        }
    };
}
fwd!(t_fileno, "fileno");
fwd!(t_isatty, "isatty");
fwd!(t_readable, "readable");
fwd!(t_writable, "writable");

fn t_seekable(o: &PyObject) -> PyResult {
    let st = check_open(o)?;
    Ok(bool_::new_bool(st.seekable))
}

fn t_reconfigure(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [encoding, errors, newline, lb, wt] =
        crate::modules::posix::kwparse::<5>("reconfigure", &["encoding", "errors", "newline", "line_buffering", "write_through"], 0, 0, 5, args, kw)?;
    let st = state(o)?;
    let started = st.decoder.is_some() && (st.snapshot.is_some() || !st.decoded.is_empty());
    let encoding = encoding.filter(|x| !x.is(none::none()));
    let errors_given = errors.filter(|x| !x.is(none::none()));
    if started && (encoding.is_some() || errors_given.is_some() || newline.is_some()) {
        return Err(unsupported("It is not possible to set the encoding or newline of stream after the first read"));
    }
    let new_newline = match newline {
        Some(n) => Some(validate_newline(Some(n))?),
        None => None,
    };
    let new_enc = match encoding {
        Some(e) => {
            if !pystr::is_str(e) {
                raise!(TypeError, "reconfigure() argument 'encoding' must be str or None, not {}", tname(e));
            }
            // validated by the codec lookup below; as in C the name ends at a NUL
            // ("locale\0" is looked up as the codec "locale", a LookupError)
            let s = pystr::as_utf8(e)?;
            Some(if s == "locale" {
                pystr::new_str("utf-8")
            } else if let Some(i) = s.find('\0') {
                pystr::new_str(&s[..i])
            } else {
                e.to_ref()
            })
        }
        None => None,
    };
    let new_errors = match errors_given {
        Some(e) => {
            if !pystr::is_str(e) {
                raise!(TypeError, "reconfigure() argument 'errors' must be str or None, not {}", tname(e));
            }
            pystr::as_utf8(e)?;
            Some(e.to_ref())
        }
        None => {
            if new_enc.is_some() {
                Some(pystr::new_str("strict"))
            } else {
                None
            }
        }
    };
    // an unknown codec must leave the stream unchanged
    if let Some(e) = &new_enc {
        lookup_codec(pystr::as_utf8(e)?)?;
    }
    call0(o, "flush")?;
    let st = state(o)?;
    if let Some(n) = new_newline {
        configure_newline(st, n);
    }
    let codec_change = new_enc.is_some() || new_errors.is_some() || newline.is_some();
    if let Some(e) = new_enc {
        st.encoding = e;
    }
    if let Some(e) = new_errors {
        st.errors = e;
    }
    if codec_change {
        let readable = st.decoder.is_some();
        let writable = st.encoder.is_some();
        build_codecs(st, readable, writable)?;
        // a new encoder in the middle of the stream must not emit a BOM
        if writable && st.seekable {
            let buf = st.buffer.clone();
            let pos = call0(&buf, "tell")?;
            if !int::is_int(&pos) || int::as_i64(&pos) != Some(0) {
                reset_encoder(state(o)?, false)?;
            }
        }
    }
    if let Some(x) = lb.filter(|x| !x.is(none::none())) {
        st.line_buffering = ab::is_true(x)?;
    }
    if let Some(x) = wt.filter(|x| !x.is(none::none())) {
        st.write_through = ab::is_true(x)?;
    }
    Ok(none::none_ref())
}

fn get_encoding(o: &PyObject) -> PyResult {
    Ok(state_any(o)?.encoding.clone())
}
fn get_errors(o: &PyObject) -> PyResult {
    Ok(state_any(o)?.errors.clone())
}
fn get_buffer(o: &PyObject) -> PyResult {
    Ok(state(o)?.buffer.clone())
}
fn get_line_buffering(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(state_any(o)?.line_buffering))
}
fn get_write_through(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(state_any(o)?.write_through))
}
fn get_closed(o: &PyObject) -> PyResult {
    let buf = state(o)?.buffer.clone();
    ab::getattr(&buf, crate::name!("closed"))
}
fn get_name(o: &PyObject) -> PyResult {
    let buf = state(o)?.buffer.clone();
    ab::getattr(&buf, crate::name!("name"))
}
fn get_newlines(o: &PyObject) -> PyResult {
    let st = state(o)?;
    match &st.decoder {
        None => Ok(none::none_ref()),
        Some(d) => match &d.nl {
            Some(nl) => Ok(newlines_obj(nl.seennl)),
            None => match &d.inner {
                Inner::Py(p) => match ab::getattr_opt(p, &pystr::intern_str("newlines"))? {
                    Some(v) => Ok(v),
                    None => Ok(none::none_ref()),
                },
                _ => Ok(none::none_ref()),
            },
        },
    }
}
fn get_chunk_size(o: &PyObject) -> PyResult {
    Ok(int::new_int(state(o)?.chunk_size as i64))
}
fn set_chunk_size(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        None => raise!(AttributeError, "_CHUNK_SIZE cannot be deleted"),
        Some(v) => v,
    };
    // out-of-range values are a ValueError like non-positive ones
    let n = ab::index_as_ssize(v, Some(crate::types::exceptions::ValueError()))?;
    if n <= 0 {
        raise!(ValueError, "a strictly positive integer is required");
    }
    state(o)?.chunk_size = n as usize;
    Ok(())
}
fn get_finalizing(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(tio(o).finalizing))
}
fn set_finalizing(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        None => raise!(TypeError, "can't delete numeric/char attribute"),
        Some(v) => {
            tio(o).finalizing = ab::is_true(v)?;
            Ok(())
        }
    }
}

fn t_getstate(o: &PyObject) -> PyResult {
    raise!(TypeError, "cannot pickle '{}' instances", crate::types::typeobject::type_name(o.ty()));
}

unsafe extern "C" fn textio_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let r = (|| -> PyResult {
            let full = crate::types::typeobject::type_fqname(o.type_ptr());
            if tio(o).st.is_null() {
                raise!(ValueError, "I/O operation on uninitialized object");
            }
            if crate::modules::reprlib_guard::enter(o) {
                raise!(RuntimeError, "reentrant call inside {}.__repr__", full);
            }
            struct G<'a>(&'a PyObject);
            impl Drop for G<'_> {
                fn drop(&mut self) {
                    crate::modules::reprlib_guard::leave(self.0);
                }
            }
            let _g = G(o);
            let mut s = format!("<{}", full);
            match ab::getattr_opt(o, crate::name!("name")) {
                Ok(Some(n)) => s.push_str(&format!(" name={}", ab::repr_string(&n)?)),
                Ok(None) => {}
                Err(e) => {
                    if exc_matches(crate::types::exceptions::ValueError()) {
                        clear_exc();
                    } else {
                        return Err(e);
                    }
                }
            }
            if let Some(m) = ab::getattr_opt(o, &pystr::intern_str("mode"))? {
                s.push_str(&format!(" mode={}", ab::repr_string(&m)?));
            }
            s.push_str(&format!(" encoding={}>", ab::repr_string(&state_any(o)?.encoding)?));
            Ok(pystr::new_str(&s))
        })();
        into_c(r)
    }
}

// ---------------------------------------------------------------- IncrementalNewlineDecoder

#[repr(C)]
pub struct NlDecoder {
    pub ob_base: PyObject,
    pub dict: *mut PyObject,
    pub decoder: *mut PyObject,
    pub errors: *mut PyObject,
    pub nl: Nl,
    pub ok: bool,
}

fn nld(o: &PyObject) -> PyResult<&mut NlDecoder> {
    let d = unsafe { &mut *(o.as_ptr() as *mut NlDecoder) };
    if !d.ok {
        raise!(ValueError, "IncrementalNewlineDecoder.__init__() not called");
    }
    Ok(d)
}

unsafe extern "C" fn nld_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe { (*tp).tp_alloc.unwrap()(tp, 0) }
}
unsafe extern "C" fn nld_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let (vals, kws) = init_args(args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            let [decoder, translate, errors] =
                kwparse::<3>("IncrementalNewlineDecoder", &["decoder", "translate", "errors"], 0, 2, &refs, kws.as_deref())?;
            let translate = ab::is_true(translate.unwrap())?;
            let d = &mut *(op as *mut NlDecoder);
            let dec = decoder.unwrap();
            set_slot(&mut d.decoder, if dec.is(none::none()) { None } else { Some(dec) });
            let e = errors.map(|e| e.to_ref()).unwrap_or_else(|| pystr::new_str("strict"));
            set_slot(&mut d.errors, Some(&e));
            d.nl = Nl { translate, pendingcr: false, seennl: 0 };
            d.ok = true;
            Ok(())
        })();
        int_into_c(r)
    }
}
unsafe extern "C" fn nld_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let d = &mut *(op as *mut NlDecoder);
        clear_slot(&mut d.decoder);
        clear_slot(&mut d.errors);
        clear_slot(&mut d.dict);
        dealloc_end(op);
    }
}
unsafe extern "C" fn nld_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let d = &*(op as *mut NlDecoder);
        let r = visit_ptr(d.decoder, visit, arg);
        if r != 0 {
            return r;
        }
        visit_ptr(d.dict, visit, arg)
    }
}

fn nld_decode(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [input, fin] = kwparse::<2>("decode", &["input", "final"], 0, 1, args, kw)?;
    let fin = match fin {
        Some(f) => ab::is_true(f)?,
        None => false,
    };
    let d = nld(o)?;
    let out = if d.decoder.is_null() {
        let i = input.unwrap();
        if !pystr::is_str(i) {
            raise!(TypeError, "decoder should return a string result, not '{}'", tname(i));
        }
        cps_of(i)
    } else {
        let dec = unsafe { PyRef::from_borrowed(d.decoder) };
        let r = ab::call_method_str(&dec, "decode", &[input.unwrap(), &bool_::new_bool(fin)])?;
        if !pystr::is_str(&r) {
            raise!(TypeError, "decoder should return a string result, not '{}'", tname(&r));
        }
        cps_of(&r)
    };
    let d = nld(o)?;
    let res = d.nl.process(out, fin);
    Ok(pystr::from_codepoints(&res))
}
fn nld_getstate(o: &PyObject) -> PyResult {
    let d = nld(o)?;
    let (buf, flag) = if d.decoder.is_null() {
        (new_bytes(&[]), 0i64)
    } else {
        let dec = unsafe { PyRef::from_borrowed(d.decoder) };
        let st = call0(&dec, "getstate")?;
        if !tuple::is_tuple(&st) || tuple::tuple_len(&st) != 2 {
            raise!(TypeError, "illegal decoder state");
        }
        (tuple::tuple_get(&st, 0).to_ref(), ab::as_i64(tuple::tuple_get(&st, 1))?)
    };
    let d = nld(o)?;
    let flag = int::new_int((flag << 1) | d.nl.pendingcr as i64);
    Ok(tuple::new_tuple(vec![buf, flag]))
}
fn nld_setstate(o: &PyObject, st: &PyObject) -> PyResult {
    if !tuple::is_tuple(st) || tuple::tuple_len(st) != 2 {
        raise!(TypeError, "state argument must be a tuple");
    }
    let flag = ab::as_i64(tuple::tuple_get(st, 1))?;
    let d = nld(o)?;
    d.nl.pendingcr = flag & 1 != 0;
    if !d.decoder.is_null() {
        let dec = unsafe { PyRef::from_borrowed(d.decoder) };
        let s = tuple::new_tuple(vec![tuple::tuple_get(st, 0).to_ref(), int::new_int(flag >> 1)]);
        call1(&dec, "setstate", &s)?;
    }
    Ok(none::none_ref())
}
fn nld_reset(o: &PyObject) -> PyResult {
    let d = nld(o)?;
    d.nl.seennl = 0;
    d.nl.pendingcr = false;
    if !d.decoder.is_null() {
        let dec = unsafe { PyRef::from_borrowed(d.decoder) };
        call0(&dec, "reset")?;
    }
    Ok(none::none_ref())
}
fn nld_newlines(o: &PyObject) -> PyResult {
    Ok(newlines_obj(nld(o)?.nl.seennl))
}

pub fn init_types() -> PyResult<()> {
    unsafe {
        if TEXTIOWRAPPER_TYPE.tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        setup_type(
            textiowrapper_type(),
            c"_io.TextIOWrapper",
            std::mem::size_of::<TextIO>(),
            super::iobase::textiobase_type(),
            "TextIOWrapper(buffer, encoding=None, errors=None, newline=None,\n              line_buffering=False, write_through=False)\n--\n\nCharacter and line based layer over a BufferedIOBase object, buffer.",
        );
        let t = &mut *textiowrapper_type();
        t.tp_new = Some(textio_new);
        t.tp_init = Some(textio_init);
        t.tp_dealloc = Some(textio_dealloc);
        t.tp_traverse = Some(textio_traverse);
        t.tp_clear = Some(textio_clear);
        t.tp_repr = Some(textio_repr);
        t.tp_iternext = Some(t_iternext);
        t.tp_methods = crate::mdefs![
            ("detach", NOARGS, t_detach),
            ("reconfigure", FASTKW, t_reconfigure, "reconfigure($self, /, *, encoding=None, errors=None, newline=None,\n            line_buffering=None, write_through=None)\n--\n\nChange the stream's text settings; omitted arguments keep their value."),
            ("write", O, t_write),
            ("read", FAST, t_read, "read($self, size=-1, /)\n--\n\nRead up to size characters; a negative size reads to EOF."),
            ("readline", FAST, t_readline, "readline($self, size=-1, /)\n--\n\nRead one line, or at most size characters of it."),
            ("flush", NOARGS, t_flush),
            ("close", NOARGS, t_close),
            ("fileno", NOARGS, t_fileno),
            ("seekable", NOARGS, t_seekable),
            ("readable", NOARGS, t_readable),
            ("writable", NOARGS, t_writable),
            ("isatty", NOARGS, t_isatty),
            ("seek", FAST, t_seek, "seek($self, cookie, whence=os.SEEK_SET, /)\n--\n\nMove to a position given by an opaque cookie from tell()."),
            ("tell", NOARGS, t_tell),
            ("truncate", FAST, t_truncate, "truncate($self, pos=None, /)\n--\n\nFlush and resize the underlying stream."),
            ("__getstate__", NOARGS, t_getstate),
        ];
        t.tp_getset = crate::getset_table![
            ("encoding", get_encoding),
            ("errors", get_errors),
            ("buffer", get_buffer),
            ("line_buffering", get_line_buffering),
            ("write_through", get_write_through),
            ("closed", get_closed),
            ("name", get_name),
            ("newlines", get_newlines),
            ("_CHUNK_SIZE", get_chunk_size, set_chunk_size),
            ("_finalizing", get_finalizing, set_finalizing),
        ];
        crate::types::typeobject::type_ready(textiowrapper_type())?;

        let t = &mut *nldecoder_type();
        t.tp_name = c"_io.IncrementalNewlineDecoder".as_ptr();
        t.tp_basicsize = std::mem::size_of::<NlDecoder>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC;
        t.tp_dictoffset = std::mem::offset_of!(NlDecoder, dict) as isize;
        t.tp_new = Some(nld_new);
        t.tp_init = Some(nld_init);
        t.tp_dealloc = Some(nld_dealloc);
        t.tp_traverse = Some(nld_traverse);
        t.tp_alloc = Some(crate::types::typeobject::generic_alloc);
        t.tp_free = Some(crate::types::typeobject::generic_free);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_setattro = Some(crate::types::typeobject::object_generic_setattro);
        t.tp_doc = c"IncrementalNewlineDecoder(decoder, translate, errors='strict')\n--\n\nCodec used when reading a file in universal newlines mode.\n\nIt wraps another incremental decoder, translating \\r\\n and \\r into \\n.\nIt also records the types of newlines encountered.  When used with\ntranslate=False, it ensures that the newline sequence is returned in\none piece. When used with decoder=None, it expects unicode strings as\ndecode input and translates newlines without first invoking an external\ndecoder.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("decode", FASTKW, nld_decode, "decode($self, /, input, final=False)\n--\n\nDecode input and translate its newlines."),
            ("getstate", NOARGS, nld_getstate),
            ("setstate", O, nld_setstate),
            ("reset", NOARGS, nld_reset),
        ];
        t.tp_getset = crate::getset_table![("newlines", nld_newlines)];
        crate::types::typeobject::type_ready(nldecoder_type())?;
    }
    Ok(())
}
