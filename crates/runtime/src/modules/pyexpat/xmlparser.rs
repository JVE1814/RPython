//! The `pyexpat` module: the `xmlparser` type and its module-level API.
//!
//! Parsing is done by the engine (engine.rs), which turns a chunk of input
//! into a batch of events.  This file delivers those events to the Python
//! handlers: character data buffering, attribute shaping (dict or ordered
//! list, specified-only), name interning, error objects and the synthetic
//! "pyexpat.c" traceback entries that tell the user which callback failed.

use super::engine::{self, Engine, Ev, Event, ModelNode};
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::compress_common::ExcCell;
use crate::modules::fncommon::{self as fc, RObj, Trav};
use crate::object::*;
use crate::types::{bool_, dict, int, list, none, str as pystr, tuple};
use crate::raise;
use std::ffi::{CString, c_int, c_void};
use std::sync::OnceLock;

pub const HANDLER_NAMES: [&str; 22] = [
    "StartElementHandler",
    "EndElementHandler",
    "ProcessingInstructionHandler",
    "CharacterDataHandler",
    "UnparsedEntityDeclHandler",
    "NotationDeclHandler",
    "StartNamespaceDeclHandler",
    "EndNamespaceDeclHandler",
    "CommentHandler",
    "StartCdataSectionHandler",
    "EndCdataSectionHandler",
    "DefaultHandler",
    "DefaultHandlerExpand",
    "NotStandaloneHandler",
    "ExternalEntityRefHandler",
    "StartDoctypeDeclHandler",
    "EndDoctypeDeclHandler",
    "EntityDeclHandler",
    "XmlDeclHandler",
    "ElementDeclHandler",
    "AttlistDeclHandler",
    "SkippedEntityHandler",
];

const H_START: usize = 0;
const H_END: usize = 1;
const H_PI: usize = 2;
const H_CHAR: usize = 3;
const H_UNPARSED: usize = 4;
const H_NOTATION: usize = 5;
const H_START_NS: usize = 6;
const H_END_NS: usize = 7;
const H_COMMENT: usize = 8;
const H_START_CDATA: usize = 9;
const H_END_CDATA: usize = 10;
const H_DEFAULT: usize = 11;
const H_DEFAULT_EXPAND: usize = 12;
const H_NOT_STANDALONE: usize = 13;
const H_EXT_REF: usize = 14;
const H_START_DOCTYPE: usize = 15;
const H_END_DOCTYPE: usize = 16;
const H_ENTITY_DECL: usize = 17;
const H_XML_DECL: usize = 18;
const H_ELEMENT_DECL: usize = 19;
const H_ATTLIST: usize = 20;
const H_SKIPPED: usize = 21;
const NH: usize = 22;

// Expat error codes used by the front end itself.
const E_EXTERNAL_ENTITY_HANDLING: u32 = 21;
const E_NOT_STANDALONE: u32 = 22;
const E_CANT_CHANGE_FEATURE_ONCE_PARSING: u32 = 26;

const DEFAULT_BUFFER_SIZE: usize = 8192;

pub struct Parser {
    engine: Option<Box<Engine>>,
    ns_sep: Option<String>,
    handlers: [Option<PyRef>; NH],
    /// pending character data while buffer_text is on
    buffer: Option<String>,
    buffer_size: usize,
    in_callback: bool,
    parent: Option<PyRef>,
    base: Option<PyRef>,
    line: u64,
    col: u64,
    byte: i64,
    err_code: u32,
    param_entity_parsing: u8,
    foreign_dtd: bool,
    reparse_deferral: bool,
    ampl: Option<(u64, f64)>,
    namespace_prefixes: bool,
    ordered_attributes: bool,
    specified_attributes: bool,
    intern: Option<PyRef>,
}

impl Trav for Parser {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        for h in self.handlers.iter() {
            let r = fc::trav_opt(v, h);
            if r != 0 {
                return r;
            }
        }
        crate::trav!(v; self.parent, self.base, self.intern)
    }
}

static mut XMLPARSER_TYPE: PyTypeObject = PyTypeObject::ZERO;
static EXPAT_ERROR: ExcCell = ExcCell::new();
static MODULE_DICT: OnceLock<usize> = OnceLock::new();

pub fn xmlparser_type() -> *mut PyTypeObject {
    &raw mut XMLPARSER_TYPE
}

#[inline(always)]
fn ps(o: &PyObject) -> &mut Parser {
    fc::rs::<Parser>(o)
}

pub fn expat_error() -> PyResult<*mut PyTypeObject> {
    EXPAT_ERROR.get_or_create("xml.parsers.expat", "ExpatError", crate::types::exceptions::Exception())
}

pub fn error_string(code: u32) -> Option<&'static str> {
    if code >= 1 && (code as usize) < engine::ERROR_MESSAGES.len() {
        Some(engine::ERROR_MESSAGES[code as usize])
    } else {
        None
    }
}

/// Build (but do not raise) an ExpatError for `code` at the given position.
pub fn make_expat_error(code: u32, line: u64, col: u64) -> PyResult<PyRef> {
    let msg = format!("{}: line {}, column {}", error_string(code).unwrap_or("unknown error"), line, col);
    let tp = expat_error()?;
    let e = ab::call(crate::types::typeobject::type_obj(tp), &[&pystr::new_str(&msg)])?;
    ab::setattr(&e, &pystr::new_str("code"), &int::new_int(code as i64))?;
    ab::setattr(&e, &pystr::new_str("lineno"), &int::new_int(line as i64))?;
    ab::setattr(&e, &pystr::new_str("offset"), &int::new_int(col as i64))?;
    Ok(e)
}

fn raise_error(o: &PyObject, code: u32, line: u64, col: u64, byte: i64) -> PyErr {
    {
        let p = ps(o);
        p.err_code = code;
        p.line = line;
        p.col = col;
        p.byte = byte;
    }
    match make_expat_error(code, line, col) {
        Ok(e) => {
            set_exc(e);
            PyErr
        }
        Err(e) => e,
    }
}

/// Prepend a traceback entry for C function `func` in "pyexpat.c" to the
/// current exception (how C callbacks show up in CPython tracebacks).
pub fn add_c_frame(func: &str) {
    let Some(e) = take_exc() else { return };
    let r = (|| -> Option<PyRef> {
        let globals = MODULE_DICT.get().map(|p| unsafe { PyRef::from_borrowed(*p as *mut PyObject) })?;
        let fname = CString::new(func).ok()?;
        unsafe {
            let code = crate::capi::funcs::PyCode_NewEmpty(c"pyexpat.c".as_ptr(), fname.as_ptr(), 1);
            if code.is_null() {
                clear_exc();
                return None;
            }
            let code = PyRef::from_raw(code);
            let frame = crate::capi::funcs::PyFrame_New(std::ptr::null_mut(), code.as_ptr(), globals.as_ptr(), std::ptr::null_mut());
            if frame.is_null() {
                clear_exc();
                return None;
            }
            Some(PyRef::from_raw(frame))
        }
    })();
    if let Some(frame) = r {
        let prev = crate::types::exceptions::exc_traceback(&e);
        let tb = crate::types::traceback::new_traceback(prev.as_deref(), &frame, -1, 1);
        crate::types::exceptions::set_exc_traceback(&e, Some(&tb));
    }
    set_exc(e);
}

fn opt_str(s: &Option<String>) -> PyRef {
    match s {
        Some(s) => pystr::new_str(s),
        None => none::none_ref(),
    }
}

/// Content model nodes -> nested (type, quant, name, children) tuples.
pub fn model_tuple(nodes: &[ModelNode], index: usize) -> PyResult {
    fc::with_recursion(" in content model", || {
        let n = &nodes[index];
        let mut kids = Vec::with_capacity(n.children.len());
        for &c in &n.children {
            kids.push(model_tuple(nodes, c)?);
        }
        Ok(tuple::new_tuple(vec![
            int::new_int(n.ctype as i64),
            int::new_int(n.quant as i64),
            opt_str(&n.name),
            tuple::new_tuple(kids),
        ]))
    })
}

// ---------------------------------------------------------------- handler calls

fn handler(o: &PyObject, idx: usize) -> Option<PyRef> {
    ps(o).handlers[idx].clone()
}

fn call_handler(o: &PyObject, idx: usize, h: &PyObject, args: &[&PyObject]) -> PyResult {
    ps(o).in_callback = true;
    let r = ab::call(h, args);
    ps(o).in_callback = false;
    if r.is_err() {
        let name = HANDLER_NAMES[idx];
        add_c_frame(name.strip_suffix("Handler").unwrap_or(name));
    }
    r
}

/// The handler for an event other than character data: pending buffered
/// text is delivered first (which may itself replace the handler).
fn handler_flushed(o: &PyObject, idx: usize) -> PyResult<Option<PyRef>> {
    if ps(o).handlers[idx].is_none() {
        return Ok(None);
    }
    flush(o)?;
    Ok(handler(o, idx))
}

fn flush(o: &PyObject) -> PyResult<()> {
    let text = match ps(o).buffer.as_mut() {
        Some(b) if !b.is_empty() => std::mem::take(b),
        _ => return Ok(()),
    };
    if let Some(h) = handler(o, H_CHAR) {
        call_handler(o, H_CHAR, &h, &[&pystr::new_str(&text)])?;
    }
    Ok(())
}

fn character_data(o: &PyObject, data: &str) -> PyResult<()> {
    if ps(o).buffer.is_none() {
        let (idx, h) = if let Some(h) = handler(o, H_CHAR) {
            (H_CHAR, h)
        } else if let Some(h) = handler(o, H_DEFAULT) {
            (H_DEFAULT, h)
        } else if let Some(h) = handler(o, H_DEFAULT_EXPAND) {
            (H_DEFAULT_EXPAND, h)
        } else {
            return Ok(());
        };
        call_handler(o, idx, &h, &[&pystr::new_str(data)])?;
        return Ok(());
    }
    let size = data.len();
    let (used, limit) = {
        let p = ps(o);
        (p.buffer.as_ref().map_or(0, |b| b.len()), p.buffer_size)
    };
    if used + size > limit {
        flush(o)?;
        if ps(o).buffer.is_none() {
            // the handler switched buffering off
            if let Some(h) = handler(o, H_CHAR) {
                call_handler(o, H_CHAR, &h, &[&pystr::new_str(data)])?;
            }
            return Ok(());
        }
    }
    if size > ps(o).buffer_size {
        if let Some(h) = handler(o, H_CHAR) {
            call_handler(o, H_CHAR, &h, &[&pystr::new_str(data)])?;
        }
    } else if let Some(b) = ps(o).buffer.as_mut() {
        b.push_str(data);
    }
    Ok(())
}

fn default_text(o: &PyObject, text: &str) -> PyResult<()> {
    let idx = if ps(o).handlers[H_DEFAULT].is_some() {
        H_DEFAULT
    } else if ps(o).handlers[H_DEFAULT_EXPAND].is_some() {
        H_DEFAULT_EXPAND
    } else {
        return Ok(());
    };
    flush(o)?;
    if let Some(h) = handler(o, idx) {
        call_handler(o, idx, &h, &[&pystr::new_str(text)])?;
    }
    Ok(())
}

fn intern_name(o: &PyObject, s: &str) -> PyResult {
    let v = pystr::new_str(s);
    match ps(o).intern.clone() {
        None => Ok(v),
        Some(d) => {
            let h = pystr::str_hash(&v);
            dict::setdefault_hash(&d, &v, h, &v)
        }
    }
}

fn invalid_return(kind: &str, value: &PyObject) -> PyErr {
    let r = ab::repr_string(value).unwrap_or_default();
    let e = match ab::call(
        crate::types::typeobject::type_obj(crate::types::exceptions::TypeError()),
        &[&pystr::new_str(&format!("invalid return value for handler: {}", r))],
    ) {
        Ok(e) => e,
        Err(e) => return e,
    };
    let note = pystr::new_str(&format!("invalid '{}' event handler return value", kind));
    if let Err(x) = ab::call_method_str(&e, "add_note", &[&note]) {
        return x;
    }
    set_exc(e);
    PyErr
}

/// Deliver a batch of events.  Returns Some(encoding name) when the engine
/// stopped to ask for a single-byte encoding table.
fn dispatch(o: &PyObject, events: Vec<Event>) -> PyResult<Option<String>> {
    for ev in events {
        {
            let p = ps(o);
            p.line = ev.line;
            p.col = ev.col;
            p.byte = ev.byte;
        }
        match ev.ev {
            Ev::CharData(t) => character_data(o, &t)?,
            Ev::StartElement { name, attrs, nspecified } => {
                let Some(h) = handler_flushed(o, H_START)? else { continue };
                let name = intern_name(o, &name)?;
                let (ordered, specified) = {
                    let p = ps(o);
                    (p.ordered_attributes, p.specified_attributes)
                };
                let n = if specified { nspecified.min(attrs.len()) } else { attrs.len() };
                let attrs_obj = if ordered {
                    let mut v = Vec::with_capacity(2 * n);
                    for (k, val) in attrs.iter().take(n) {
                        v.push(intern_name(o, k)?);
                        v.push(pystr::new_str(val));
                    }
                    list::new_list(v)
                } else {
                    let d = dict::new_dict_presized(n);
                    for (k, val) in attrs.iter().take(n) {
                        let key = intern_name(o, k)?;
                        dict::set_item(&d, &key, &pystr::new_str(val))?;
                    }
                    d
                };
                call_handler(o, H_START, &h, &[&name, &attrs_obj])?;
            }
            Ev::EndElement { name } => {
                if let Some(h) = handler_flushed(o, H_END)? {
                    let name = intern_name(o, &name)?;
                    call_handler(o, H_END, &h, &[&name])?;
                }
            }
            Ev::Error(code) => {
                flush(o)?;
                return Err(raise_error(o, code, ev.line, ev.col, ev.byte));
            }
            Ev::NeedEncoding(name) => return Ok(Some(name)),
            Ev::Default(t) => default_text(o, &t)?,
            other => dispatch_other(o, other, &ev_pos(ev.line, ev.col, ev.byte))?,
        }
    }
    Ok(None)
}

struct Pos(u64, u64, i64);
fn ev_pos(l: u64, c: u64, b: i64) -> Pos {
    Pos(l, c, b)
}

fn dispatch_other(o: &PyObject, ev: Ev, pos: &Pos) -> PyResult<()> {
    match ev {
        Ev::XmlDecl { version, encoding, standalone } => {
            if let Some(h) = handler_flushed(o, H_XML_DECL)? {
                call_handler(o, H_XML_DECL, &h, &[&opt_str(&version), &opt_str(&encoding), &int::new_int(standalone as i64)])?;
            }
        }
        Ev::StartDoctype { name, sysid, pubid, internal } => {
            if let Some(h) = handler_flushed(o, H_START_DOCTYPE)? {
                call_handler(
                    o,
                    H_START_DOCTYPE,
                    &h,
                    &[&pystr::new_str(&name), &opt_str(&sysid), &opt_str(&pubid), &int::new_int(internal as i64)],
                )?;
            }
        }
        Ev::EndDoctype => {
            if let Some(h) = handler_flushed(o, H_END_DOCTYPE)? {
                call_handler(o, H_END_DOCTYPE, &h, &[])?;
            }
        }
        Ev::ElementDecl { name, model } => {
            if let Some(h) = handler_flushed(o, H_ELEMENT_DECL)? {
                let m = model_tuple(&model, 0)?;
                call_handler(o, H_ELEMENT_DECL, &h, &[&pystr::new_str(&name), &m])?;
            }
        }
        Ev::AttlistDecl { elname, attname, atype, default, required } => {
            if let Some(h) = handler_flushed(o, H_ATTLIST)? {
                call_handler(
                    o,
                    H_ATTLIST,
                    &h,
                    &[
                        &pystr::new_str(&elname),
                        &pystr::new_str(&attname),
                        &pystr::new_str(&atype),
                        &opt_str(&default),
                        &int::new_int(required as i64),
                    ],
                )?;
            }
        }
        Ev::EntityDecl { name, is_pe, value, base, sysid, pubid, notation } => {
            if notation.is_some() && ps(o).handlers[H_UNPARSED].is_some() {
                if let Some(h) = handler_flushed(o, H_UNPARSED)? {
                    call_handler(
                        o,
                        H_UNPARSED,
                        &h,
                        &[&pystr::new_str(&name), &opt_str(&base), &opt_str(&sysid), &opt_str(&pubid), &opt_str(&notation)],
                    )?;
                }
                return Ok(());
            }
            if let Some(h) = handler_flushed(o, H_ENTITY_DECL)? {
                call_handler(
                    o,
                    H_ENTITY_DECL,
                    &h,
                    &[
                        &pystr::new_str(&name),
                        &int::new_int(is_pe as i64),
                        &opt_str(&value),
                        &opt_str(&base),
                        &opt_str(&sysid),
                        &opt_str(&pubid),
                        &opt_str(&notation),
                    ],
                )?;
            }
        }
        Ev::NotationDecl { name, base, sysid, pubid } => {
            if let Some(h) = handler_flushed(o, H_NOTATION)? {
                call_handler(o, H_NOTATION, &h, &[&pystr::new_str(&name), &opt_str(&base), &opt_str(&sysid), &opt_str(&pubid)])?;
            }
        }
        Ev::StartNs { prefix, uri } => {
            if let Some(h) = handler_flushed(o, H_START_NS)? {
                call_handler(o, H_START_NS, &h, &[&opt_str(&prefix), &opt_str(&uri)])?;
            }
        }
        Ev::EndNs { prefix } => {
            if let Some(h) = handler_flushed(o, H_END_NS)? {
                call_handler(o, H_END_NS, &h, &[&opt_str(&prefix)])?;
            }
        }
        Ev::Pi { target, data } => {
            if let Some(h) = handler_flushed(o, H_PI)? {
                call_handler(o, H_PI, &h, &[&pystr::new_str(&target), &pystr::new_str(&data)])?;
            } else if data.is_empty() {
                default_text(o, &format!("<?{}?>", target))?;
            } else {
                default_text(o, &format!("<?{} {}?>", target, data))?;
            }
        }
        Ev::Comment(t) => {
            if let Some(h) = handler_flushed(o, H_COMMENT)? {
                call_handler(o, H_COMMENT, &h, &[&pystr::new_str(&t)])?;
            } else {
                default_text(o, &format!("<!--{}-->", t))?;
            }
        }
        Ev::StartCdata => {
            if let Some(h) = handler_flushed(o, H_START_CDATA)? {
                call_handler(o, H_START_CDATA, &h, &[])?;
            }
        }
        Ev::EndCdata => {
            if let Some(h) = handler_flushed(o, H_END_CDATA)? {
                call_handler(o, H_END_CDATA, &h, &[])?;
            }
        }
        Ev::SkippedEntity { name, is_pe } => {
            if let Some(h) = handler_flushed(o, H_SKIPPED)? {
                call_handler(o, H_SKIPPED, &h, &[&pystr::new_str(&name), &int::new_int(is_pe as i64)])?;
            } else if is_pe {
                default_text(o, &format!("%{};", name))?;
            } else {
                default_text(o, &format!("&{};", name))?;
            }
        }
        Ev::ExternalEntityRef { context, base, sysid, pubid } => {
            if let Some(h) = handler_flushed(o, H_EXT_REF)? {
                let r = call_handler(o, H_EXT_REF, &h, &[&opt_str(&context), &opt_str(&base), &opt_str(&sysid), &opt_str(&pubid)])?;
                if !int::is_int(&r) {
                    return Err(invalid_return("ExternalEntityRef", &r));
                }
                if !ab::is_true(&r)? {
                    return Err(raise_error(o, E_EXTERNAL_ENTITY_HANDLING, pos.0, pos.1, pos.2));
                }
            } else if let Some(c) = context {
                default_text(o, &format!("&{};", c))?;
            }
        }
        Ev::NotStandalone => {
            if let Some(h) = handler_flushed(o, H_NOT_STANDALONE)? {
                let r = call_handler(o, H_NOT_STANDALONE, &h, &[])?;
                if !int::is_int(&r) {
                    return Err(invalid_return("NotStandalone", &r));
                }
                if !ab::is_true(&r)? {
                    return Err(raise_error(o, E_NOT_STANDALONE, pos.0, pos.1, pos.2));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------- feeding

fn configure(o: &PyObject) -> PyResult<&mut Engine> {
    let p = ps(o);
    let Some(eng) = p.engine.as_deref_mut() else {
        raise!(ValueError, "the parser is not initialized");
    };
    eng.ns_triplets = p.namespace_prefixes;
    eng.param_entity_parsing = p.param_entity_parsing;
    eng.use_foreign_dtd = p.foreign_dtd;
    eng.reparse_deferral = p.reparse_deferral;
    eng.base = match &p.base {
        Some(b) if pystr::is_str(b) => Some(pystr::to_string_lossy(b)),
        _ => None,
    };
    if let Some((thr, mx)) = p.ampl {
        eng.ampl_threshold = thr;
        eng.ampl_max = mx;
    }
    Ok(eng)
}

/// A 256-entry table for a single-byte encoding known to the codecs.
pub fn encoding_table(name: &str) -> PyResult<Vec<u32>> {
    let all: Vec<u8> = (0..=255u8).collect();
    let b = crate::types::bytes::new_bytes(&all);
    let s = ab::call(
        crate::types::typeobject::type_obj(pystr::str_type()),
        &[&b, &pystr::new_str(name), &pystr::new_str("replace")],
    )?;
    if pystr::str_len(&s) != 256 {
        raise!(ValueError, "multi-byte encodings are not supported");
    }
    let s = pystr::to_string_lossy(&s);
    Ok(s.chars().map(|c| if c == '\u{FFFD}' { u32::MAX } else { c as u32 }).collect())
}

fn feed(o: &PyObject, data: &[u8], final_: bool) -> PyResult<()> {
    let mut events = configure(o)?.feed(data, final_);
    loop {
        let need = dispatch(o, events)?;
        let Some(name) = need else { break };
        match encoding_table(&name) {
            Ok(t) => events = ps(o).engine.as_deref_mut().unwrap().set_encoding_table(t),
            Err(e) => {
                ps(o).engine.as_deref_mut().unwrap().encoding_failed();
                return Err(e);
            }
        }
    }
    flush(o)
}

// ---------------------------------------------------------------- methods

fn check_not_in_callback(o: &PyObject, what: &str) -> PyResult<()> {
    if ps(o).in_callback {
        raise!(RuntimeError, "cannot call {}() from within a handler", what);
    }
    Ok(())
}

fn m_parse(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("Parse", args, 1, 2)?;
    check_not_in_callback(o, "Parse")?;
    let isfinal = match args.get(1) {
        Some(x) => ab::is_true(x)?,
        None => false,
    };
    let data = args[0];
    if pystr::is_str(data) {
        let started = ps(o).engine.as_ref().map_or(false, |e| e.is_started());
        if !started {
            if let Some(e) = ps(o).engine.as_deref_mut() {
                e.force_encoding("utf-8".to_string());
            }
        }
        let b = ab::call_method_str(data, "encode", &[&pystr::new_str("utf-8"), &pystr::new_str("surrogatepass")])?;
        let bytes = crate::buffer::buffer_bytes(&b).unwrap_or_default();
        feed(o, &bytes, isfinal)?;
    } else {
        let Some(bytes) = crate::buffer::buffer_bytes(data) else {
            raise!(TypeError, "a bytes-like object is required, not '{}'", fc::tname(data));
        };
        feed(o, &bytes, isfinal)?;
    }
    Ok(int::new_int(1))
}

fn m_parse_file(o: &PyObject, file: &PyObject) -> PyResult {
    check_not_in_callback(o, "ParseFile")?;
    let read = ab::getattr_str(file, "read")?;
    let n = int::new_int(2048);
    loop {
        let chunk = ab::call(&read, &[&n])?;
        let bytes = if crate::types::bytes::is_bytes(&chunk) || crate::types::bytearray::is_bytearray(&chunk) {
            crate::buffer::buffer_bytes(&chunk).unwrap_or_default()
        } else {
            raise!(TypeError, "read() did not return a bytes object (type={:.400})", fc::tname(&chunk));
        };
        if bytes.is_empty() {
            feed(o, b"", true)?;
            return Ok(int::new_int(1));
        }
        feed(o, &bytes, false)?;
    }
}

fn m_set_base(o: &PyObject, base: &PyObject) -> PyResult {
    if !pystr::is_str(base) {
        raise!(TypeError, "SetBase() argument must be str, not {}", fc::tname(base));
    }
    ps(o).base = Some(base.to_ref());
    Ok(none::none_ref())
}

fn m_get_base(o: &PyObject) -> PyResult {
    Ok(ps(o).base.clone().unwrap_or_else(none::none_ref))
}

fn m_get_input_context(_o: &PyObject) -> PyResult {
    Ok(none::none_ref())
}

fn new_parser(
    tp: *mut PyTypeObject,
    engine: Engine,
    ns_sep: Option<String>,
    intern: Option<PyRef>,
    parent: Option<PyRef>,
) -> PyResult {
    let p = Parser {
        engine: Some(Box::new(engine)),
        ns_sep,
        handlers: Default::default(),
        buffer: None,
        buffer_size: DEFAULT_BUFFER_SIZE,
        in_callback: false,
        parent,
        base: None,
        line: 1,
        col: 0,
        byte: -1,
        err_code: 0,
        param_entity_parsing: 0,
        foreign_dtd: false,
        reparse_deferral: true,
        ampl: None,
        namespace_prefixes: false,
        ordered_attributes: false,
        specified_attributes: false,
        intern,
    };
    let o = fc::robj_new(tp, p)?;
    Ok(o)
}

fn m_external_entity_parser_create(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("ExternalEntityParserCreate", args, 1, 2)?;
    let ctx = args[0];
    let context = if ctx.is(none::none()) {
        None
    } else if pystr::is_str(ctx) {
        Some(pystr::to_string_lossy(ctx))
    } else {
        raise!(TypeError, "ExternalEntityParserCreate() argument 1 must be str or None, not {}", fc::tname(ctx));
    };
    let encoding = match args.get(1) {
        None => None,
        Some(e) if e.is(none::none()) => None,
        Some(e) if pystr::is_str(e) => Some(pystr::to_string_lossy(e)),
        Some(e) => raise!(TypeError, "ExternalEntityParserCreate() argument 2 must be str, not {}", fc::tname(e)),
    };
    let eng = configure(o)?;
    let sub_engine = Engine::new_external(eng, context, encoding);
    let (ns_sep, intern) = {
        let p = ps(o);
        (p.ns_sep.clone(), p.intern.clone())
    };
    let sub = new_parser(o.type_ptr(), sub_engine, ns_sep, intern, Some(o.to_ref()))?;
    let src = ps(o);
    let dst = ps(&sub);
    dst.handlers = src.handlers.clone();
    dst.buffer = src.buffer.as_ref().map(|_| String::new());
    dst.buffer_size = src.buffer_size;
    dst.namespace_prefixes = src.namespace_prefixes;
    dst.ordered_attributes = src.ordered_attributes;
    dst.specified_attributes = src.specified_attributes;
    dst.base = src.base.clone();
    dst.param_entity_parsing = src.param_entity_parsing;
    dst.reparse_deferral = src.reparse_deferral;
    Ok(sub)
}

fn m_set_param_entity_parsing(o: &PyObject, flag: &PyObject) -> PyResult {
    let f = ab::as_i64(flag)?;
    if !(0..=2).contains(&f) {
        return Ok(int::new_int(0));
    }
    ps(o).param_entity_parsing = f as u8;
    Ok(int::new_int(1))
}

fn m_use_foreign_dtd(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("UseForeignDTD", args, 0, 1)?;
    let flag = match args.first() {
        Some(x) => ab::is_true(x)?,
        None => true,
    };
    if ps(o).engine.as_ref().map_or(false, |e| e.is_started()) {
        let (l, c, b) = {
            let p = ps(o);
            (p.line, p.col, p.byte)
        };
        return Err(raise_error(o, E_CANT_CHANGE_FEATURE_ONCE_PARSING, l, c, b));
    }
    ps(o).foreign_dtd = flag;
    Ok(none::none_ref())
}

fn m_get_reparse_deferral(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(ps(o).reparse_deferral))
}

fn m_set_reparse_deferral(o: &PyObject, enabled: &PyObject) -> PyResult {
    ps(o).reparse_deferral = ab::is_true(enabled)?;
    Ok(none::none_ref())
}

fn check_root(o: &PyObject) -> PyResult<()> {
    if ps(o).parent.is_some() {
        let tp = expat_error()?;
        let e = ab::call(crate::types::typeobject::type_obj(tp), &[&pystr::new_str("parser must be a root parser")])?;
        set_exc(e);
        return Err(PyErr);
    }
    Ok(())
}

const DEFAULT_AMPL: (u64, f64) = (8 * 1024 * 1024, 100.0);

fn m_set_ampl_threshold(o: &PyObject, threshold: &PyObject) -> PyResult {
    if !int::is_int(threshold) {
        raise!(TypeError, "threshold must be an integer");
    }
    let v = ab::as_i128_index(threshold)?;
    if v < 0 {
        raise!(ValueError, "threshold must be non-negative");
    }
    if v > u64::MAX as i128 {
        raise!(OverflowError, "threshold is too large");
    }
    check_root(o)?;
    let cur = ps(o).ampl.unwrap_or(DEFAULT_AMPL);
    ps(o).ampl = Some(((v as u64).min(i64::MAX as u64), cur.1));
    Ok(none::none_ref())
}

fn m_set_ampl_max(o: &PyObject, factor: &PyObject) -> PyResult {
    if bool_::is_bool(factor) || !(int::is_int(factor) || crate::types::float::is_float(factor)) {
        raise!(TypeError, "max_factor must be a float");
    }
    let f = ab::float_value(factor)?;
    check_root(o)?;
    if !(f >= 1.0) {
        let tp = expat_error()?;
        let e = ab::call(crate::types::typeobject::type_obj(tp), &[&pystr::new_str("'max_factor' must be at least 1.0")])?;
        set_exc(e);
        return Err(PyErr);
    }
    let cur = ps(o).ampl.unwrap_or(DEFAULT_AMPL);
    ps(o).ampl = Some((cur.0, f));
    Ok(none::none_ref())
}

// ---------------------------------------------------------------- attributes

unsafe extern "C" fn get_handler(s: *mut PyObject, closure: *mut c_void) -> *mut PyObject {
    unsafe {
        let p = ps(&*s);
        match &p.handlers[closure as usize] {
            Some(h) => h.clone().into_raw(),
            None => none::none_ref().into_raw(),
        }
    }
}

unsafe extern "C" fn set_handler(s: *mut PyObject, v: *mut PyObject, closure: *mut c_void) -> c_int {
    unsafe {
        let o = &*s;
        if v.is_null() {
            let _ = err!(AttributeError, "cannot delete attribute");
            return -1;
        }
        let idx = closure as usize;
        if idx == H_CHAR {
            if flush(o).is_err() {
                return -1;
            }
        }
        let v = &*v;
        let new = if v.is(none::none()) { None } else { Some(v.to_ref()) };
        let old = std::mem::replace(&mut ps(o).handlers[idx], new);
        drop(old);
        0
    }
}

const F_BUFFER_TEXT: usize = 0;
const F_NS_PREFIXES: usize = 1;
const F_ORDERED: usize = 2;
const F_SPECIFIED: usize = 3;

unsafe extern "C" fn get_flag(s: *mut PyObject, closure: *mut c_void) -> *mut PyObject {
    unsafe {
        let p = ps(&*s);
        let v = match closure as usize {
            F_BUFFER_TEXT => p.buffer.is_some(),
            F_NS_PREFIXES => p.namespace_prefixes,
            F_ORDERED => p.ordered_attributes,
            _ => p.specified_attributes,
        };
        bool_::new_bool(v).into_raw()
    }
}

unsafe extern "C" fn set_flag(s: *mut PyObject, v: *mut PyObject, closure: *mut c_void) -> c_int {
    unsafe {
        let o = &*s;
        if v.is_null() {
            let _ = err!(AttributeError, "cannot delete attribute");
            return -1;
        }
        let b = match ab::is_true(&*v) {
            Ok(b) => b,
            Err(_) => return -1,
        };
        match closure as usize {
            F_BUFFER_TEXT => {
                if b {
                    if ps(o).buffer.is_none() {
                        ps(o).buffer = Some(String::new());
                    }
                } else if ps(o).buffer.is_some() {
                    if flush(o).is_err() {
                        return -1;
                    }
                    ps(o).buffer = None;
                }
            }
            F_NS_PREFIXES => ps(o).namespace_prefixes = b,
            F_ORDERED => ps(o).ordered_attributes = b,
            _ => ps(o).specified_attributes = b,
        }
        0
    }
}

fn get_buffer_size(o: &PyObject) -> PyResult {
    Ok(int::new_int(ps(o).buffer_size as i64))
}

fn set_buffer_size(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let Some(v) = v else { raise!(AttributeError, "cannot delete attribute") };
    if !int::is_int(v) {
        raise!(TypeError, "buffer_size must be an integer");
    }
    let n = ab::as_i128_index(v)?;
    if n <= 0 {
        raise!(ValueError, "buffer_size must be greater than zero");
    }
    if n > i32::MAX as i128 {
        raise!(ValueError, "buffer_size must not be greater than {}", i32::MAX);
    }
    if ps(o).buffer.is_some() && n as usize != ps(o).buffer_size {
        flush(o)?;
    }
    ps(o).buffer_size = n as usize;
    Ok(())
}

fn get_buffer_used(o: &PyObject) -> PyResult {
    Ok(int::new_int(ps(o).buffer.as_ref().map_or(0, |b| b.len()) as i64))
}

fn get_intern(o: &PyObject) -> PyResult {
    Ok(ps(o).intern.clone().unwrap_or_else(none::none_ref))
}

fn get_error_code(o: &PyObject) -> PyResult {
    Ok(int::new_int(ps(o).err_code as i64))
}
fn get_line(o: &PyObject) -> PyResult {
    Ok(int::new_int(ps(o).line as i64))
}
fn get_col(o: &PyObject) -> PyResult {
    Ok(int::new_int(ps(o).col as i64))
}
fn get_byte(o: &PyObject) -> PyResult {
    Ok(int::new_int(ps(o).byte))
}

unsafe extern "C" fn parser_clear(o: *mut PyObject) -> c_int {
    unsafe {
        let p = ps(&*o);
        let hs: Vec<Option<PyRef>> = p.handlers.iter_mut().map(|h| h.take()).collect();
        let a = p.parent.take();
        let b = p.base.take();
        let c = p.intern.take();
        drop((hs, a, b, c));
        0
    }
}

unsafe extern "C" fn parser_setattro(o: *mut PyObject, name: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        if !pystr::is_str(&*name) {
            let _ = err!(TypeError, "attribute name must be string, not '{}'", fc::tname(&*name));
            return -1;
        }
        crate::types::typeobject::object_generic_setattro(o, name, v)
    }
}

fn errors_module() -> PyResult {
    let m = crate::types::module::new_module("pyexpat.errors")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str("Constants used to describe error conditions."))?;
    let codes = dict::new_dict();
    let messages = dict::new_dict();
    for code in 1..engine::ERROR_NAMES.len() {
        let msg = pystr::new_str(engine::ERROR_MESSAGES[code]);
        dict::set_item_str(&d, engine::ERROR_NAMES[code], &msg)?;
        dict::set_item(&codes, &msg, &int::new_int(code as i64))?;
        dict::set_item(&messages, &int::new_int(code as i64), &msg)?;
    }
    dict::set_item_str(&d, "codes", &codes)?;
    dict::set_item_str(&d, "messages", &messages)?;
    Ok(m)
}

fn model_module() -> PyResult {
    let m = crate::types::module::new_module("pyexpat.model")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str("Constants used to interpret content model information."))?;
    for (n, v) in [
        ("XML_CTYPE_EMPTY", 1),
        ("XML_CTYPE_ANY", 2),
        ("XML_CTYPE_MIXED", 3),
        ("XML_CTYPE_NAME", 4),
        ("XML_CTYPE_CHOICE", 5),
        ("XML_CTYPE_SEQ", 6),
        ("XML_CQUANT_NONE", 0),
        ("XML_CQUANT_OPT", 1),
        ("XML_CQUANT_REP", 2),
        ("XML_CQUANT_PLUS", 3),
    ] {
        dict::set_item_str(&d, n, &int::new_int(v))?;
    }
    Ok(m)
}

fn f_error_string(_m: &PyObject, code: &PyObject) -> PyResult {
    let c = ab::as_i64(code)?;
    if c < 0 || c > u32::MAX as i64 {
        return Ok(none::none_ref());
    }
    Ok(match error_string(c as u32) {
        Some(s) => pystr::new_str(s),
        None => none::none_ref(),
    })
}

fn f_parser_create(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("ParserCreate", &["encoding", "namespace_separator", "intern"], 0, 0);
    let [enc, sep, intern] = crate::args::parse::<3>(&P, args, kw)?;
    let encoding = match enc {
        None => None,
        Some(e) if e.is(none::none()) => None,
        Some(e) if pystr::is_str(e) => Some(pystr::to_string_lossy(e)),
        Some(e) => raise!(TypeError, "ParserCreate() argument 'encoding' must be str or None, not {}", fc::tname(e)),
    };
    let ns_sep = match sep {
        None => None,
        Some(s) if s.is(none::none()) => None,
        Some(s) if pystr::is_str(s) => {
            if pystr::str_len(s) > 1 {
                raise!(ValueError, "namespace_separator must be at most one character, omitted, or None");
            }
            Some(pystr::to_string_lossy(s))
        }
        Some(s) => raise!(TypeError, "ParserCreate() argument 'namespace_separator' must be str or None, not {}", fc::tname(s)),
    };
    let intern = match intern {
        None => Some(dict::new_dict()),
        Some(i) if i.is(none::none()) => None,
        Some(i) if dict::is_dict(i) => Some(i.to_ref()),
        Some(_) => raise!(TypeError, "intern must be a dictionary"),
    };
    let eng = Engine::new(ns_sep.clone(), encoding);
    new_parser(xmlparser_type(), eng, ns_sep, intern, None)
}

fn init_type() -> PyResult<()> {
    unsafe {
        let t = xmlparser_type();
        if (*t).tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        fc::init_robj_type::<Parser>(t, c"pyexpat.xmlparser", Py_TPFLAGS_DISALLOW_INSTANTIATION);
        (*t).tp_clear = Some(parser_clear);
        (*t).tp_setattro = Some(parser_setattro);
        (*t).tp_doc = c"XML parser".as_ptr();
        (*t).tp_methods = crate::mdefs![
            ("Parse", FAST, m_parse, "Parse XML data.\n\n'isfinal' should be true at end of input."),
            ("ParseFile", O, m_parse_file, "Parse XML data from file-like object."),
            ("SetBase", O, m_set_base, "Set the base URL for the parser."),
            ("GetBase", NOARGS, m_get_base, "Return base URL string for the parser."),
            ("GetInputContext", NOARGS, m_get_input_context, "Return the untranslated text of the input that caused the current event."),
            ("ExternalEntityParserCreate", FAST, m_external_entity_parser_create, "Create a parser for parsing an external entity based on the information passed to the ExternalEntityRefHandler."),
            ("SetParamEntityParsing", O, m_set_param_entity_parsing, "Controls parsing of parameter entities (including the external DTD subset)."),
            ("UseForeignDTD", FAST, m_use_foreign_dtd, "Allows the application to provide an artificial external subset if one is not specified as part of the document instance."),
            ("GetReparseDeferralEnabled", NOARGS, m_get_reparse_deferral, "Retrieve reparse deferral enabled status; always returns false with Expat <2.6.0."),
            ("SetReparseDeferralEnabled", O, m_set_reparse_deferral, "Enable/Disable reparse deferral; enabled by default with Expat >=2.6.0."),
            ("SetBillionLaughsAttackProtectionActivationThreshold", O, m_set_ampl_threshold, "Sets the number of output bytes needed to activate protection against billion laughs attacks."),
            ("SetBillionLaughsAttackProtectionMaximumAmplification", O, m_set_ampl_max, "Sets the maximum tolerated amplification factor for protection against billion laughs attacks."),
        ];
        let mut gs: Vec<PyGetSetDef> = Vec::new();
        for (i, n) in HANDLER_NAMES.iter().enumerate() {
            let mut d = crate::methods::gsdef(n, Some(get_handler), Some(set_handler), "");
            d.closure = i as *mut c_void;
            gs.push(d);
        }
        for (i, n) in ["buffer_text", "namespace_prefixes", "ordered_attributes", "specified_attributes"].iter().enumerate() {
            let mut d = crate::methods::gsdef(n, Some(get_flag), Some(set_flag), "");
            d.closure = i as *mut c_void;
            gs.push(d);
        }
        let extra = crate::getset_table![
            ("buffer_size", get_buffer_size, set_buffer_size),
            ("buffer_used", get_buffer_used),
            ("intern", get_intern),
            ("ErrorCode", get_error_code),
            ("ErrorLineNumber", get_line),
            ("ErrorColumnNumber", get_col),
            ("ErrorByteIndex", get_byte),
            ("CurrentLineNumber", get_line),
            ("CurrentColumnNumber", get_col),
            ("CurrentByteIndex", get_byte),
        ];
        let mut k = 0;
        while !(*extra.add(k)).name.is_null() {
            gs.push(std::ptr::read(extra.add(k)));
            k += 1;
        }
        (*t).tp_getset = crate::methods::gstable(gs);
        crate::types::typeobject::type_ready(t)?;
    }
    Ok(())
}

pub fn init() -> PyResult {
    init_type()?;
    let m = crate::types::module::new_module("pyexpat")?;
    let d = crate::types::module::module_dict(&m);
    let _ = MODULE_DICT.get_or_init(|| d.clone().into_raw() as usize);
    dict::set_item_str(&d, "__doc__", &pystr::new_str("Python wrapper for Expat parser."))?;
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("ParserCreate", FASTKW, f_parser_create, "Return a new XML parser object."),
            ("ErrorString", O, f_error_string, "Returns string error for given number."),
        ],
        Some(&m),
    )?;
    let err = crate::types::typeobject::type_obj(expat_error()?);
    dict::set_item_str(&d, "ExpatError", err)?;
    dict::set_item_str(&d, "error", err)?;
    dict::set_item_str(&d, "XMLParserType", crate::types::typeobject::type_obj(xmlparser_type()))?;
    dict::set_item_str(&d, "EXPAT_VERSION", &pystr::new_str("expat_2.7.3"))?;
    dict::set_item_str(
        &d,
        "version_info",
        &tuple::new_tuple(vec![int::new_int(2), int::new_int(7), int::new_int(3)]),
    )?;
    dict::set_item_str(&d, "native_encoding", &pystr::new_str("UTF-8"))?;
    let features: Vec<PyRef> = [
        ("sizeof(XML_Char)", 1),
        ("sizeof(XML_LChar)", 1),
        ("XML_DTD", 0),
        ("XML_CONTEXT_BYTES", 1024),
        ("XML_NS", 0),
        ("XML_GE", 0),
    ]
    .iter()
    .map(|(n, v)| tuple::tuple2(pystr::new_str(n), int::new_int(*v)))
    .collect();
    dict::set_item_str(&d, "features", &list::new_list(features))?;
    dict::set_item_str(&d, "XML_PARAM_ENTITY_PARSING_NEVER", &int::new_int(0))?;
    dict::set_item_str(&d, "XML_PARAM_ENTITY_PARSING_UNLESS_STANDALONE", &int::new_int(1))?;
    dict::set_item_str(&d, "XML_PARAM_ENTITY_PARSING_ALWAYS", &int::new_int(2))?;
    let errors = errors_module()?;
    let model = model_module()?;
    dict::set_item_str(&d, "errors", &errors)?;
    dict::set_item_str(&d, "model", &model)?;
    let modules = crate::modules::sys::modules_dict();
    dict::set_item_str(&modules, "pyexpat.errors", &errors)?;
    dict::set_item_str(&modules, "pyexpat.model", &model)?;
    Ok(m)
}
