//! A non-validating XML 1.0 parser (with namespaces) producing a stream of
//! events, written for the `pyexpat` module.  Its observable behaviour
//! (event order, error codes, positions) follows Expat, which is what the
//! Python API exposes.
//!
//! The engine is incremental: `feed()` may be called with arbitrary pieces
//! of the document.  Constructs that are not complete yet are kept in the
//! buffer until more data arrives (or the final piece is fed).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// error codes (numbering of Expat's XML_Error enumeration)

pub const E_NO_MEMORY: u32 = 1;
pub const E_SYNTAX: u32 = 2;
pub const E_NO_ELEMENTS: u32 = 3;
pub const E_INVALID_TOKEN: u32 = 4;
pub const E_UNCLOSED_TOKEN: u32 = 5;
pub const E_PARTIAL_CHAR: u32 = 6;
pub const E_TAG_MISMATCH: u32 = 7;
pub const E_DUPLICATE_ATTRIBUTE: u32 = 8;
pub const E_JUNK_AFTER_DOC_ELEMENT: u32 = 9;
pub const E_PARAM_ENTITY_REF: u32 = 10;
pub const E_UNDEFINED_ENTITY: u32 = 11;
pub const E_RECURSIVE_ENTITY_REF: u32 = 12;
pub const E_ASYNC_ENTITY: u32 = 13;
pub const E_BAD_CHAR_REF: u32 = 14;
pub const E_BINARY_ENTITY_REF: u32 = 15;
pub const E_ATTRIBUTE_EXTERNAL_ENTITY_REF: u32 = 16;
pub const E_MISPLACED_XML_PI: u32 = 17;
pub const E_UNKNOWN_ENCODING: u32 = 18;
pub const E_INCORRECT_ENCODING: u32 = 19;
pub const E_UNCLOSED_CDATA_SECTION: u32 = 20;
pub const E_UNBOUND_PREFIX: u32 = 27;
pub const E_UNDECLARING_PREFIX: u32 = 28;
pub const E_XML_DECL: u32 = 30;
pub const E_TEXT_DECL: u32 = 31;
pub const E_PUBLICID: u32 = 32;
pub const E_FINISHED: u32 = 36;
pub const E_RESERVED_PREFIX_XML: u32 = 38;
pub const E_RESERVED_PREFIX_XMLNS: u32 = 39;
pub const E_RESERVED_NAMESPACE_URI: u32 = 40;
pub const E_AMPLIFICATION_LIMIT_BREACH: u32 = 43;

/// Memory every parser holds before any input (allocation limiter model).
const PARSER_BASE_BYTES: u64 = 1024;

pub const ERROR_MESSAGES: &[&str] = &[
    "",
    "out of memory",
    "syntax error",
    "no element found",
    "not well-formed (invalid token)",
    "unclosed token",
    "partial character",
    "mismatched tag",
    "duplicate attribute",
    "junk after document element",
    "illegal parameter entity reference",
    "undefined entity",
    "recursive entity reference",
    "asynchronous entity",
    "reference to invalid character number",
    "reference to binary entity",
    "reference to external entity in attribute",
    "XML or text declaration not at start of entity",
    "unknown encoding",
    "encoding specified in XML declaration is incorrect",
    "unclosed CDATA section",
    "error in processing external entity reference",
    "document is not standalone",
    "unexpected parser state - please send a bug report",
    "entity declared in parameter entity",
    "requested feature requires XML_DTD support in Expat",
    "cannot change setting once parsing has begun",
    "unbound prefix",
    "must not undeclare prefix",
    "incomplete markup in parameter entity",
    "XML declaration not well-formed",
    "text declaration not well-formed",
    "illegal character(s) in public id",
    "parser suspended",
    "parser not suspended",
    "parsing aborted",
    "parsing finished",
    "cannot suspend in external parameter entity",
    "reserved prefix (xml) must not be undeclared or bound to another namespace name",
    "reserved prefix (xmlns) must not be declared or undeclared",
    "prefix must not be bound to one of the reserved namespace names",
    "invalid argument",
    "a successful prior call to function XML_GetBuffer is required",
    "limit on input amplification factor (from DTD and entities) breached",
    "parser not started",
];

pub const ERROR_NAMES: &[&str] = &[
    "",
    "XML_ERROR_NO_MEMORY",
    "XML_ERROR_SYNTAX",
    "XML_ERROR_NO_ELEMENTS",
    "XML_ERROR_INVALID_TOKEN",
    "XML_ERROR_UNCLOSED_TOKEN",
    "XML_ERROR_PARTIAL_CHAR",
    "XML_ERROR_TAG_MISMATCH",
    "XML_ERROR_DUPLICATE_ATTRIBUTE",
    "XML_ERROR_JUNK_AFTER_DOC_ELEMENT",
    "XML_ERROR_PARAM_ENTITY_REF",
    "XML_ERROR_UNDEFINED_ENTITY",
    "XML_ERROR_RECURSIVE_ENTITY_REF",
    "XML_ERROR_ASYNC_ENTITY",
    "XML_ERROR_BAD_CHAR_REF",
    "XML_ERROR_BINARY_ENTITY_REF",
    "XML_ERROR_ATTRIBUTE_EXTERNAL_ENTITY_REF",
    "XML_ERROR_MISPLACED_XML_PI",
    "XML_ERROR_UNKNOWN_ENCODING",
    "XML_ERROR_INCORRECT_ENCODING",
    "XML_ERROR_UNCLOSED_CDATA_SECTION",
    "XML_ERROR_EXTERNAL_ENTITY_HANDLING",
    "XML_ERROR_NOT_STANDALONE",
    "XML_ERROR_UNEXPECTED_STATE",
    "XML_ERROR_ENTITY_DECLARED_IN_PE",
    "XML_ERROR_FEATURE_REQUIRES_XML_DTD",
    "XML_ERROR_CANT_CHANGE_FEATURE_ONCE_PARSING",
    "XML_ERROR_UNBOUND_PREFIX",
    "XML_ERROR_UNDECLARING_PREFIX",
    "XML_ERROR_INCOMPLETE_PE",
    "XML_ERROR_XML_DECL",
    "XML_ERROR_TEXT_DECL",
    "XML_ERROR_PUBLICID",
    "XML_ERROR_SUSPENDED",
    "XML_ERROR_NOT_SUSPENDED",
    "XML_ERROR_ABORTED",
    "XML_ERROR_FINISHED",
    "XML_ERROR_SUSPEND_PE",
    "XML_ERROR_RESERVED_PREFIX_XML",
    "XML_ERROR_RESERVED_PREFIX_XMLNS",
    "XML_ERROR_RESERVED_NAMESPACE_URI",
    "XML_ERROR_INVALID_ARGUMENT",
    "XML_ERROR_NO_BUFFER",
    "XML_ERROR_AMPLIFICATION_LIMIT_BREACH",
    "XML_ERROR_NOT_STARTED",
];

const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";

// ---------------------------------------------------------------------------
// events

/// One node of an element content model, stored in a flat vector (models
/// can be nested very deeply).
pub struct ModelNode {
    pub ctype: u8,
    pub quant: u8,
    pub name: Option<String>,
    pub children: Vec<usize>,
}

// The payloads of rare events are boxed so that Ev (and the event batches)
// stay small.
pub struct StartDoctypeEv {
    pub name: String,
    pub sysid: Option<String>,
    pub pubid: Option<String>,
    pub internal: bool,
}
pub struct AttlistDeclEv {
    pub elname: String,
    pub attname: String,
    pub atype: String,
    pub default: Option<String>,
    pub required: bool,
}
pub struct EntityDeclEv {
    pub name: String,
    pub is_pe: bool,
    pub value: Option<String>,
    pub base: Option<String>,
    pub sysid: Option<String>,
    pub pubid: Option<String>,
    pub notation: Option<String>,
}
pub struct NotationDeclEv {
    pub name: String,
    pub base: Option<String>,
    pub sysid: Option<String>,
    pub pubid: Option<String>,
}
pub struct ExternalEntityRefEv {
    pub context: Option<String>,
    pub base: Option<String>,
    pub sysid: Option<String>,
    pub pubid: Option<String>,
}

pub enum Ev {
    XmlDecl { version: Option<String>, encoding: Option<String>, standalone: i32 },
    StartDoctype(Box<StartDoctypeEv>),
    EndDoctype,
    ElementDecl { name: String, model: Vec<ModelNode> },
    AttlistDecl(Box<AttlistDeclEv>),
    EntityDecl(Box<EntityDeclEv>),
    NotationDecl(Box<NotationDeclEv>),
    StartElement { name: String, attrs: Vec<(String, String)>, nspecified: usize },
    EndElement { name: String },
    StartNs { prefix: Option<String>, uri: Option<String> },
    EndNs { prefix: Option<String> },
    CharData(String),
    Pi { target: String, data: String },
    Comment(String),
    StartCdata,
    EndCdata,
    Default(String),
    SkippedEntity { name: String, is_pe: bool },
    ExternalEntityRef(Box<ExternalEntityRefEv>),
    NotStandalone,
    NeedEncoding(String),
    Error(u32),
}

pub struct Event {
    pub ev: Ev,
    pub line: u64,
    pub col: u64,
    pub byte: i64,
}

// ---------------------------------------------------------------------------
// DTD information shared with external entity parsers

#[derive(Clone)]
pub struct Entity {
    pub text: Option<Arc<Vec<char>>>,
    pub sysid: Option<String>,
    pub pubid: Option<String>,
    pub base: Option<String>,
    pub notation: Option<String>,
}

#[derive(Clone)]
pub struct AttDef {
    pub name: String,
    pub is_cdata: bool,
    pub default: Option<String>,
}

#[derive(Default)]
pub struct Dtd {
    pub general: HashMap<String, Entity>,
    pub params: HashMap<String, Entity>,
    pub attlists: HashMap<String, Vec<AttDef>>,
    pub standalone: bool,
    pub has_pe_refs: bool,
    pub keep_processing: bool,
}

// ---------------------------------------------------------------------------
// character classes

pub fn is_name_start(c: char) -> bool {
    matches!(c, ':' | 'A'..='Z' | '_' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// ASCII name characters: bit 0 = may start a name, bit 1 = may continue one.
static ASCII_NAME: [u8; 128] = {
    let mut t = [0u8; 128];
    let mut c = 0;
    while c < 128 {
        let ch = c as u8;
        let start = ch.is_ascii_alphabetic() || ch == b'_' || ch == b':';
        let more = start || ch.is_ascii_digit() || ch == b'-' || ch == b'.';
        t[c] = (start as u8) | ((more as u8) << 1);
        c += 1;
    }
    t
};

#[inline]
pub fn is_name_char(c: char) -> bool {
    if (c as u32) < 128 {
        return ASCII_NAME[c as usize] & 2 != 0;
    }
    is_name_start(c) || matches!(c, '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn is_pubid_char(c: char) -> bool {
    matches!(c, ' ' | '\r' | '\n' | 'a'..='z' | 'A'..='Z' | '0'..='9')
        || "-'()+,./:=?;!*#@$_%".contains(c)
}

// ---------------------------------------------------------------------------
// input decoding

#[derive(Clone)]
pub enum Enc {
    Detect,
    Utf8,
    Utf16Le,
    Utf16Be,
    Latin1,
    Ascii,
    Table(Arc<Vec<u32>>),
}

fn known_encoding(name: &str) -> Option<Enc> {
    match name.to_ascii_uppercase().as_str() {
        "UTF-8" => Some(Enc::Utf8),
        "UTF-16" => Some(Enc::Detect),
        "UTF-16LE" => Some(Enc::Utf16Le),
        "UTF-16BE" => Some(Enc::Utf16Be),
        "ISO-8859-1" => Some(Enc::Latin1),
        "US-ASCII" => Some(Enc::Ascii),
        _ => None,
    }
}

type R<T> = Result<T, (u32, usize)>;

#[derive(Clone, Copy, PartialEq)]
enum State {
    Prolog,
    Subset,
    Content,
    Epilog,
    ExtSubset,
    ExtContent,
}

struct Frame {
    raw_name: String,
    name: String,
    ns_declared: Vec<(Option<String>, Option<String>)>, // (prefix, previous binding)
}

pub struct Engine {
    // configuration
    pub ns_sep: Option<String>,
    pub ns_triplets: bool,
    pub param_entity_parsing: u8,
    pub use_foreign_dtd: bool,
    pub reparse_deferral: bool,
    pub ampl_threshold: u64,
    pub ampl_max: f64,
    /// Allocation limiter: once the memory the parser holds (a model:
    /// buffered input, DTD storage, a fixed base) reaches `alloc_threshold`
    /// bytes, it may be at most `alloc_max` times the input fed so far.
    pub alloc_threshold: u64,
    pub alloc_max: f64,
    alloc_bytes: u64,
    pub base: Option<String>,
    pub forced_encoding: Option<String>,

    // input
    raw: Vec<u8>,
    enc: Enc,
    enc_resolved: bool,
    utf16_detect_done: bool,
    buf: Vec<char>,
    widths: Vec<u8>,
    pos: usize,
    final_: bool,
    pending_cr_width: u8,

    // position tracking for buf[track_idx]
    track_idx: usize,
    track_line: u64,
    track_col: u64,
    track_byte: i64,

    // parse state
    state: State,
    started: bool,
    finished: bool,
    errored: bool,
    awaiting_encoding: bool,
    seen_doctype: bool,
    doctype_extid: Option<(Option<String>, Option<String>)>,
    root_seen: bool,
    stack: Vec<Frame>,
    ns_bindings: HashMap<String, Vec<String>>, // prefix ("" = default) -> stack of uris
    pub dtd: Arc<Mutex<Dtd>>,
    open_entities: Vec<String>,
    is_external: bool,
    ext_context: Option<String>,
    decl_allowed: bool,
    last_partial: usize,

    // amplification accounting
    direct_bytes: u64,
    indirect_bytes: u64,

    events: Vec<Event>,
    ev_override: Option<(u64, u64, i64)>,
}

fn new_dtd() -> Dtd {
    Dtd { keep_processing: true, ..Default::default() }
}

impl Engine {
    pub fn new(ns_sep: Option<String>, encoding: Option<String>) -> Engine {
        Engine {
            ns_sep,
            ns_triplets: false,
            param_entity_parsing: 0,
            use_foreign_dtd: false,
            reparse_deferral: true,
            ampl_threshold: 8 * 1024 * 1024,
            ampl_max: 100.0,
            alloc_threshold: 64 * 1024 * 1024,
            alloc_max: 100.0,
            alloc_bytes: PARSER_BASE_BYTES,
            base: None,
            forced_encoding: encoding,
            raw: Vec::new(),
            enc: Enc::Detect,
            enc_resolved: false,
            utf16_detect_done: false,
            buf: Vec::new(),
            widths: Vec::new(),
            pos: 0,
            final_: false,
            pending_cr_width: 0,
            track_idx: 0,
            track_line: 1,
            track_col: 0,
            track_byte: 0,
            state: State::Prolog,
            started: false,
            finished: false,
            errored: false,
            awaiting_encoding: false,
            seen_doctype: false,
            doctype_extid: None,
            root_seen: false,
            stack: Vec::new(),
            ns_bindings: HashMap::new(),
            dtd: Arc::new(Mutex::new(new_dtd())),
            open_entities: Vec::new(),
            is_external: false,
            ext_context: None,
            decl_allowed: true,
            last_partial: 0,
            direct_bytes: 0,
            indirect_bytes: 0,
            events: Vec::new(),
            ev_override: None,
        }
    }

    /// An engine for an external entity (context Some) or for the external
    /// DTD subset (context None), sharing the DTD of `parent`.
    pub fn new_external(parent: &Engine, context: Option<String>, encoding: Option<String>) -> Engine {
        let mut e = Engine::new(parent.ns_sep.clone(), encoding);
        e.ns_triplets = parent.ns_triplets;
        e.param_entity_parsing = parent.param_entity_parsing;
        e.reparse_deferral = parent.reparse_deferral;
        e.ampl_threshold = parent.ampl_threshold;
        e.ampl_max = parent.ampl_max;
        e.alloc_threshold = parent.alloc_threshold;
        e.alloc_max = parent.alloc_max;
        e.base = parent.base.clone();
        e.dtd = parent.dtd.clone();
        e.is_external = true;
        e.state = if context.is_some() { State::ExtContent } else { State::ExtSubset };
        e.ns_bindings = parent.ns_bindings.clone();
        e.ext_context = context;
        e
    }

    pub fn force_encoding(&mut self, name: String) {
        if !self.started {
            self.forced_encoding = Some(name);
        }
    }

    pub fn is_started(&self) -> bool {
        self.started
    }

    pub fn current_position(&mut self) -> (u64, u64, i64) {
        let p = self.pos;
        self.position_at(p)
    }

    // -- events -------------------------------------------------------------

    fn position_at(&mut self, idx: usize) -> (u64, u64, i64) {
        if idx < self.track_idx {
            return (self.track_line, self.track_col, self.track_byte);
        }
        let end = idx.min(self.buf.len());
        for i in self.track_idx..end {
            let c = self.buf[i];
            self.track_byte += self.widths[i] as i64;
            if c == '\n' {
                self.track_line += 1;
                self.track_col = 0;
            } else {
                self.track_col += 1;
            }
        }
        self.track_idx = end;
        (self.track_line, self.track_col, self.track_byte)
    }

    fn emit_at(&mut self, idx: usize, ev: Ev) {
        let (line, col, byte) = match self.ev_override {
            Some(p) => p,
            None => self.position_at(idx),
        };
        self.events.push(Event { ev, line, col, byte });
    }

    fn fail(&mut self, code: u32, idx: usize) {
        let (line, col, byte) = match self.ev_override {
            Some(p) => p,
            None => self.position_at(idx),
        };
        self.errored = true;
        self.events.push(Event { ev: Ev::Error(code), line, col, byte });
    }

    // -- feeding ------------------------------------------------------------

    /// Feed more input; returns the events produced.  The last event is an
    /// Error or NeedEncoding event if parsing stopped for that reason.
    pub fn feed(&mut self, data: &[u8], final_: bool) -> Vec<Event> {
        self.events.clear();
        if self.errored {
            return std::mem::take(&mut self.events);
        }
        if self.finished {
            let p = self.buf.len();
            self.fail(E_FINISHED, p);
            return std::mem::take(&mut self.events);
        }
        self.started = true;
        // roughly one event per dozen input bytes in typical documents
        self.events.reserve((data.len() / 12).min(1 << 16));
        self.raw.extend_from_slice(data);
        self.direct_bytes += data.len() as u64;
        self.final_ = final_;
        if self.track_alloc(data.len()).is_err() {
            let p = self.pos;
            self.fail(E_NO_MEMORY, p);
            return std::mem::take(&mut self.events);
        }
        if self.awaiting_encoding {
            return std::mem::take(&mut self.events);
        }
        if self.reparse_deferral && !final_ && self.last_partial > 0 && data.len() > 0 {
            let available = self.raw.len() + self.buf.len() - self.pos;
            if available < 2 * self.last_partial {
                return std::mem::take(&mut self.events);
            }
        }
        self.run();
        std::mem::take(&mut self.events)
    }

    /// Supply the byte -> code point table for an encoding announced with a
    /// NeedEncoding event (u32::MAX marks invalid bytes), then continue.
    pub fn set_encoding_table(&mut self, table: Vec<u32>) -> Vec<Event> {
        self.events.clear();
        self.awaiting_encoding = false;
        // ASCII characters must map to themselves.
        for i in 0..128u32 {
            let c = char::from_u32(i).unwrap();
            if (i >= 0x20 && i < 0x7f) || c == '\t' || c == '\n' || c == '\r' {
                if table[i as usize] != i {
                    let p = self.pos;
                    self.fail(E_UNKNOWN_ENCODING, p);
                    return std::mem::take(&mut self.events);
                }
            }
        }
        for (i, &v) in table.iter().enumerate() {
            if v < 0x80 && v as usize != i {
                let c = char::from_u32(v).unwrap();
                if (v >= 0x20 && v < 0x7f) || c == '\t' || c == '\n' || c == '\r' {
                    let p = self.pos;
                    self.fail(E_UNKNOWN_ENCODING, p);
                    return std::mem::take(&mut self.events);
                }
            }
        }
        self.enc = Enc::Table(Arc::new(table));
        self.enc_resolved = true;
        self.run();
        std::mem::take(&mut self.events)
    }

    pub fn encoding_failed(&mut self) {
        self.awaiting_encoding = false;
        self.errored = true;
    }

    fn run(&mut self) {
        let run_start = self.pos;
        if let Err((code, idx)) = self.decode() {
            self.fail(code, idx);
            return;
        }
        if self.awaiting_encoding {
            return;
        }
        loop {
            let before = self.pos;
            match self.step() {
                Ok(true) => {
                    if self.awaiting_encoding {
                        return;
                    }
                    if !self.raw.is_empty() {
                        if let Err((code, idx)) = self.decode() {
                            self.fail(code, idx);
                            return;
                        }
                        if self.awaiting_encoding {
                            return;
                        }
                    }
                    if self.pos == before && !self.awaiting_encoding {
                        // safety net against loops without progress
                        if self.pos >= self.buf.len() {
                            break;
                        }
                    }
                    continue;
                }
                Ok(false) => {
                    if !self.raw.is_empty() && self.enc_resolved && !self.awaiting_encoding {
                        let before_len = self.buf.len();
                        if let Err((code, idx)) = self.decode() {
                            self.fail(code, idx);
                            return;
                        }
                        if self.buf.len() > before_len {
                            continue;
                        }
                    }
                    if self.pos == run_start && (self.pos < self.buf.len() || !self.raw.is_empty()) {
                        // like Expat, defer only after an attempt that consumed nothing
                        self.last_partial = self.buf.len() - self.pos + self.raw.len();
                    } else {
                        self.last_partial = 0;
                    }
                    if self.final_ {
                        self.at_eof();
                    }
                    break;
                }
                Err((code, idx)) => {
                    self.fail(code, idx);
                    return;
                }
            }
        }
        self.compact();
    }

    fn compact(&mut self) {
        if self.pos > 4096 && self.pos == self.track_idx {
            self.buf.drain(..self.pos);
            self.widths.drain(..self.pos);
            self.track_idx -= self.pos;
            self.pos = 0;
        }
    }

    fn at_eof(&mut self) {
        if !self.raw.is_empty() {
            // a CR held back for CRLF folding still counts as a line end
            self.flush_cr(true);
            let p = self.buf.len();
            let code = if matches!(self.enc, Enc::Utf16Le | Enc::Utf16Be) { E_UNCLOSED_TOKEN } else { E_PARTIAL_CHAR };
            self.fail(code, p);
            return;
        }
        let end = self.buf.len();
        match self.state {
            State::Prolog => {
                if self.pos < end {
                    let p = self.pos;
                    self.fail(E_UNCLOSED_TOKEN, p);
                } else {
                    self.fail(E_NO_ELEMENTS, end);
                }
            }
            State::Subset => {
                let p = self.pos;
                self.fail(E_UNCLOSED_TOKEN, p);
            }
            State::Content => {
                if self.pos < end {
                    let p = self.pos;
                    self.fail(E_UNCLOSED_TOKEN, p);
                } else {
                    self.fail(E_NO_ELEMENTS, end);
                }
            }
            State::ExtContent => {
                if self.pos < end {
                    let p = self.pos;
                    self.fail(E_UNCLOSED_TOKEN, p);
                } else if !self.stack.is_empty() {
                    self.fail(E_ASYNC_ENTITY, end);
                } else {
                    self.finished = true;
                }
            }
            State::ExtSubset => {
                if self.pos < end {
                    let p = self.pos;
                    self.fail(E_UNCLOSED_TOKEN, p);
                } else {
                    self.finished = true;
                }
            }
            State::Epilog => {
                if self.pos < end {
                    let p = self.pos;
                    self.fail(E_UNCLOSED_TOKEN, p);
                } else {
                    self.finished = true;
                }
            }
        }
    }

    // -- decoding -----------------------------------------------------------

    fn push_char(&mut self, c: char, width: u8) {
        // newline normalisation: CR LF and lone CR become LF
        if self.pending_cr_width > 0 {
            let w = self.pending_cr_width;
            self.pending_cr_width = 0;
            if c == '\n' {
                self.buf.push('\n');
                self.widths.push(w + width);
                return;
            }
            self.buf.push('\n');
            self.widths.push(w);
        }
        if c == '\r' {
            self.pending_cr_width = width;
            return;
        }
        self.buf.push(c);
        self.widths.push(width);
    }

    fn flush_cr(&mut self, final_: bool) {
        if final_ && self.pending_cr_width > 0 {
            let w = self.pending_cr_width;
            self.pending_cr_width = 0;
            self.buf.push('\n');
            self.widths.push(w);
        }
    }

    fn decode(&mut self) -> R<()> {
        if !self.utf16_detect_done {
            if let Some(name) = self.forced_encoding.clone() {
                self.utf16_detect_done = true;
                match known_encoding(&name) {
                    Some(Enc::Detect) => {
                        // UTF-16: BOM decides, default big endian
                        if self.raw.len() < 2 && !self.final_ {
                            self.utf16_detect_done = false;
                            return Ok(());
                        }
                        if self.raw.starts_with(&[0xFF, 0xFE]) {
                            self.raw.drain(..2);
                            self.track_byte += 2;
                            self.enc = Enc::Utf16Le;
                        } else {
                            if self.raw.starts_with(&[0xFE, 0xFF]) {
                                self.raw.drain(..2);
                                self.track_byte += 2;
                            }
                            self.enc = Enc::Utf16Be;
                        }
                    }
                    Some(e) => {
                        self.enc = e;
                        if matches!(self.enc, Enc::Utf8) && self.raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
                            self.raw.drain(..3);
                            self.track_byte += 3;
                        }
                    }
                    None => {
                        self.awaiting_encoding = true;
                        let p = self.pos;
                        self.emit_at(p, Ev::NeedEncoding(name));
                        return Ok(());
                    }
                }
                self.enc_resolved = true;
            } else {
                let r = &self.raw;
                if r.len() < 2 && !self.final_ {
                    return Ok(());
                }
                self.utf16_detect_done = true;
                if r.starts_with(&[0xFE, 0xFF]) {
                    self.raw.drain(..2);
                    self.track_byte += 2;
                    self.enc = Enc::Utf16Be;
                } else if r.starts_with(&[0xFF, 0xFE]) {
                    self.raw.drain(..2);
                    self.track_byte += 2;
                    self.enc = Enc::Utf16Le;
                } else if r.len() >= 2 && r[0] == 0 {
                    self.enc = Enc::Utf16Be;
                } else if r.len() >= 2 && r[1] == 0 {
                    self.enc = Enc::Utf16Le;
                } else if r.starts_with(&[0xEF, 0xBB, 0xBF]) {
                    self.raw.drain(..3);
                    self.track_byte += 3;
                    self.enc = Enc::Utf8;
                    self.enc_resolved = false;
                } else {
                    self.enc = Enc::Utf8;
                    self.enc_resolved = false;
                }
                if matches!(self.enc, Enc::Utf16Be | Enc::Utf16Le) {
                    // the declaration cannot change a UTF-16 encoding
                    self.enc_resolved = true;
                }
            }
        }
        let final_ = self.final_;
        let raw = std::mem::take(&mut self.raw);
        let mut i = 0;
        let n = raw.len();
        let provisional = !self.enc_resolved;
        let enc = self.enc.clone();
        let mut result = Ok(());
        while i < n {
            match &enc {
                Enc::Utf8 | Enc::Detect => {
                    let b = raw[i];
                    if b < 0x80 {
                        if b != b'\r' && self.pending_cr_width == 0 {
                            // a run of plain ASCII needs no per-character work
                            let mut j = i + 1;
                            while j < n && raw[j] < 0x80 && raw[j] != b'\r' {
                                j += 1;
                            }
                            self.buf.extend(raw[i..j].iter().map(|&x| x as char));
                            self.widths.resize(self.widths.len() + (j - i), 1);
                            i = j;
                            continue;
                        }
                        self.push_char(b as char, 1);
                        i += 1;
                        continue;
                    }
                    if provisional {
                        break;
                    }
                    let need = if b >= 0xF0 && b <= 0xF4 {
                        4
                    } else if b >= 0xE0 {
                        3
                    } else if b >= 0xC2 && b < 0xE0 {
                        2
                    } else {
                        result = Err((E_INVALID_TOKEN, self.buf.len()));
                        break;
                    };
                    if b > 0xF4 {
                        result = Err((E_INVALID_TOKEN, self.buf.len()));
                        break;
                    }
                    if i + need > n {
                        // check what we have is a valid prefix
                        for k in i + 1..n {
                            if raw[k] & 0xC0 != 0x80 {
                                result = Err((E_INVALID_TOKEN, self.buf.len()));
                                break;
                            }
                        }
                        if result.is_err() {
                            break;
                        }
                        if final_ {
                            result = Err((E_PARTIAL_CHAR, self.buf.len()));
                        }
                        break;
                    }
                    match std::str::from_utf8(&raw[i..i + need]) {
                        Ok(s) => {
                            let c = s.chars().next().unwrap();
                            self.push_char(c, need as u8);
                            i += need;
                        }
                        Err(_) => {
                            result = Err((E_INVALID_TOKEN, self.buf.len()));
                            break;
                        }
                    }
                }
                Enc::Latin1 => {
                    self.push_char(raw[i] as char, 1);
                    i += 1;
                }
                Enc::Ascii => {
                    if raw[i] >= 0x80 {
                        result = Err((E_INVALID_TOKEN, self.buf.len()));
                        break;
                    }
                    self.push_char(raw[i] as char, 1);
                    i += 1;
                }
                Enc::Table(t) => {
                    let v = t[raw[i] as usize];
                    match char::from_u32(v) {
                        Some(c) if v != u32::MAX => {
                            self.push_char(c, 1);
                            i += 1;
                        }
                        _ => {
                            result = Err((E_INVALID_TOKEN, self.buf.len()));
                            break;
                        }
                    }
                }
                Enc::Utf16Le | Enc::Utf16Be => {
                    if i + 2 > n {
                        // a lone trailing byte is reported by at_eof
                        break;
                    }
                    let le = matches!(enc, Enc::Utf16Le);
                    let unit = |k: usize| -> u32 {
                        if le {
                            raw[k] as u32 | (raw[k + 1] as u32) << 8
                        } else {
                            (raw[k] as u32) << 8 | raw[k + 1] as u32
                        }
                    };
                    let u = unit(i);
                    if (0xD800..0xDC00).contains(&u) {
                        if i + 4 > n {
                            if final_ {
                                result = Err((E_PARTIAL_CHAR, self.buf.len()));
                            }
                            break;
                        }
                        let u2 = unit(i + 2);
                        if !(0xDC00..0xE000).contains(&u2) {
                            result = Err((E_INVALID_TOKEN, self.buf.len()));
                            break;
                        }
                        let c = char::from_u32(0x10000 + ((u - 0xD800) << 10) + (u2 - 0xDC00)).unwrap();
                        self.push_char(c, 4);
                        i += 4;
                    } else if (0xDC00..0xE000).contains(&u) {
                        result = Err((E_INVALID_TOKEN, self.buf.len()));
                        break;
                    } else {
                        self.push_char(char::from_u32(u).unwrap(), 2);
                        i += 2;
                    }
                }
            }
        }
        self.raw = raw[i..].to_vec();
        if self.raw.is_empty() || result.is_err() {
            self.flush_cr(final_);
        }
        result
    }

    /// Called when the encoding question is settled (declaration parsed or
    /// known to be absent).
    fn resolve_encoding(&mut self, declared: Option<&str>, at: usize) -> R<()> {
        if self.enc_resolved {
            if let Some(name) = declared {
                // a UTF-16 document may not claim to be in an 8-bit encoding
                let upper = name.to_ascii_uppercase();
                if matches!(self.enc, Enc::Utf16Be | Enc::Utf16Le)
                    && self.forced_encoding.is_none()
                    && !upper.starts_with("UTF-16")
                    && !upper.starts_with("UTF16")
                {
                    return Err((E_INCORRECT_ENCODING, at));
                }
            }
            return Ok(());
        }
        self.enc_resolved = true;
        let name = match declared {
            None => return Ok(()),
            Some(n) => n,
        };
        match known_encoding(name) {
            Some(Enc::Utf8) | Some(Enc::Ascii) if self.forced_encoding.is_none() => {
                self.enc = known_encoding(name).unwrap();
            }
            Some(Enc::Latin1) => self.enc = Enc::Latin1,
            Some(Enc::Detect) | Some(Enc::Utf16Le) | Some(Enc::Utf16Be) => {
                return Err((E_INCORRECT_ENCODING, at));
            }
            Some(_) => {}
            None => {
                self.awaiting_encoding = true;
                self.emit_at(at, Ev::NeedEncoding(name.to_string()));
            }
        }
        Ok(())
    }

    // -- helpers ------------------------------------------------------------

    fn ch(&self, i: usize) -> Option<char> {
        self.buf.get(i).copied()
    }

    fn starts_with(&self, i: usize, s: &str) -> Option<bool> {
        // Some(true) if buf[i..] starts with s, Some(false) if it cannot,
        // None if there is not enough data yet.
        let mut k = i;
        for c in s.chars() {
            match self.buf.get(k) {
                None => return if self.final_ { Some(false) } else { None },
                Some(&b) if b != c => return Some(false),
                _ => {}
            }
            k += 1;
        }
        Some(true)
    }

    fn text(&self, a: usize, b: usize) -> String {
        self.buf[a..b].iter().collect()
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while i < self.buf.len() && is_ws(self.buf[i]) {
            i += 1;
        }
        i
    }

    fn scan_name(&self, i: usize) -> R<Option<usize>> {
        // returns end of name, None if more data needed
        match self.ch(i) {
            None => return if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(None) },
            Some(c) if !is_name_start(c) => return Err((E_INVALID_TOKEN, i)),
            _ => {}
        }
        let mut k = i + 1;
        while let Some(c) = self.ch(k) {
            if !is_name_char(c) {
                return Ok(Some(k));
            }
            k += 1;
        }
        if self.final_ {
            Ok(Some(k))
        } else {
            Ok(None)
        }
    }

    /// Find the end ('>') of a markup construct starting at `i`, skipping
    /// quoted strings.  Returns the index of '>'.
    fn find_tag_end(&self, i: usize) -> Option<usize> {
        let mut quote: Option<char> = None;
        let mut k = i;
        while k < self.buf.len() {
            let c = self.buf[k];
            match quote {
                Some(q) => {
                    if c == q {
                        quote = None;
                    }
                }
                None => {
                    if c == '"' || c == '\'' {
                        quote = Some(c);
                    } else if c == '>' {
                        return Some(k);
                    }
                }
            }
            k += 1;
        }
        None
    }

    fn find_str(&self, i: usize, s: &str) -> Option<usize> {
        let pat: Vec<char> = s.chars().collect();
        if self.buf.len() < pat.len() {
            return None;
        }
        let mut k = i;
        while k + pat.len() <= self.buf.len() {
            if self.buf[k] == pat[0] && self.buf[k..k + pat.len()] == pat[..] {
                return Some(k);
            }
            k += 1;
        }
        None
    }

    fn check_chars(&self, a: usize, b: usize) -> R<()> {
        for k in a..b {
            if !is_xml_char(self.buf[k]) {
                return Err((E_INVALID_TOKEN, k));
            }
        }
        Ok(())
    }

    /// Record `bytes` more held by the parser; Err if the allocation
    /// limiter trips.
    fn track_alloc(&mut self, bytes: usize) -> Result<(), ()> {
        self.alloc_bytes += bytes as u64;
        if self.alloc_bytes >= self.alloc_threshold && self.direct_bytes > 0 {
            let factor = self.alloc_bytes as f64 / self.direct_bytes as f64;
            if factor > self.alloc_max {
                return Err(());
            }
        }
        Ok(())
    }

    fn account(&mut self, chars: usize, at: usize) -> R<()> {
        self.indirect_bytes += chars as u64;
        let total = self.direct_bytes + self.indirect_bytes;
        if total >= self.ampl_threshold && self.direct_bytes > 0 {
            let factor = total as f64 / self.direct_bytes as f64;
            if factor > self.ampl_max {
                return Err((E_AMPLIFICATION_LIMIT_BREACH, at));
            }
        }
        Ok(())
    }

    // -- main dispatcher ----------------------------------------------------

    fn step(&mut self) -> R<bool> {
        if self.pos >= self.buf.len() {
            return Ok(false);
        }
        match self.state {
            State::Prolog | State::Epilog => self.step_misc(),
            State::Subset | State::ExtSubset => self.step_subset(),
            State::Content | State::ExtContent => self.step_content(),
        }
    }

    fn try_decl(&mut self) -> R<Option<bool>> {
        // XML (or text) declaration at the very start of the input.
        // Returns Some(true) if handled, Some(false) if there is none,
        // None if more data is needed.
        self.decl_allowed = false;
        let start = self.pos;
        match self.starts_with(start, "<?xml") {
            None => {
                self.decl_allowed = true;
                return Ok(None);
            }
            Some(false) => {
                self.resolve_encoding(None, start)?;
                return Ok(Some(false));
            }
            Some(true) => {}
        }
        match self.ch(start + 5) {
            None => {
                if self.final_ {
                    return Err((E_UNCLOSED_TOKEN, start));
                }
                self.decl_allowed = true;
                return Ok(None);
            }
            Some(c) if !is_ws(c) => {
                // '<?xml-stylesheet' etc.: a processing instruction
                self.resolve_encoding(None, start)?;
                return Ok(Some(false));
            }
            _ => {}
        }
        let end = match self.find_str(start, "?>") {
            Some(e) => e,
            None => {
                if !self.enc_resolved && !self.raw.is_empty() {
                    // non-ASCII text inside the declaration
                    return Err((if self.is_external { E_TEXT_DECL } else { E_XML_DECL }, self.buf.len()));
                }
                if self.final_ {
                    return Err((E_UNCLOSED_TOKEN, start));
                }
                self.decl_allowed = true;
                return Ok(None);
            }
        };
        let is_text_decl = self.is_external;
        let err = if is_text_decl { E_TEXT_DECL } else { E_XML_DECL };
        // parse pseudo attributes
        let mut k = start + 5;
        let mut version = None;
        let mut encoding = None;
        let mut standalone = -1;
        let mut order = 0;
        loop {
            let ws_start = k;
            k = self.skip_ws(k);
            if k >= end {
                break;
            }
            if k == ws_start {
                return Err((err, k));
            }
            let ns = k;
            while k < end && self.buf[k].is_ascii_alphabetic() {
                k += 1;
            }
            let name = self.text(ns, k);
            k = self.skip_ws(k);
            if k >= end || self.buf[k] != '=' {
                return Err((err, k));
            }
            k = self.skip_ws(k + 1);
            if k >= end || (self.buf[k] != '"' && self.buf[k] != '\'') {
                return Err((err, k));
            }
            let q = self.buf[k];
            let vs = k + 1;
            let mut ve = vs;
            while ve < end && self.buf[ve] != q {
                ve += 1;
            }
            if ve >= end {
                return Err((err, k));
            }
            let value = self.text(vs, ve);
            k = ve + 1;
            match name.as_str() {
                "version" if order == 0 => {
                    if value.is_empty()
                        || !value.chars().all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
                    {
                        return Err((err, vs));
                    }
                    version = Some(value);
                    order = 1;
                }
                "encoding" if order <= 1 && (order == 1 || is_text_decl) => {
                    let ok = value.chars().next().map_or(false, |c| c.is_ascii_alphabetic())
                        && value.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
                    if !ok {
                        return Err((err, vs));
                    }
                    encoding = Some(value);
                    order = 2;
                }
                "standalone" if (order == 1 || order == 2) && !is_text_decl => {
                    standalone = match value.as_str() {
                        "yes" => 1,
                        "no" => 0,
                        _ => return Err((err, vs)),
                    };
                    order = 3;
                }
                _ => return Err((err, ns)),
            }
        }
        if version.is_none() && !is_text_decl {
            return Err((err, start));
        }
        if is_text_decl && encoding.is_none() {
            return Err((err, start));
        }
        if standalone == 1 {
            self.dtd.lock().unwrap().standalone = true;
        }
        self.emit_at(start, Ev::XmlDecl { version, encoding: encoding.clone(), standalone });
        self.pos = end + 2;
        self.resolve_encoding(encoding.as_deref(), start)?;
        Ok(Some(true))
    }

    fn step_misc(&mut self) -> R<bool> {
        let i = self.pos;
        if self.decl_allowed {
            match self.try_decl()? {
                None => return Ok(false),
                Some(_) => return Ok(true),
            }
        }
        let c = self.buf[i];
        if is_ws(c) {
            let mut k = i;
            while k < self.buf.len() && is_ws(self.buf[k]) {
                k += 1;
            }
            let t = self.text(i, k);
            self.emit_at(i, Ev::Default(t));
            self.pos = k;
            return Ok(true);
        }
        if c != '<' {
            if !is_xml_char(c) {
                return Err((E_INVALID_TOKEN, i));
            }
            if is_name_char(c) {
                // a name token: Expat only recognises it once it is
                // terminated; an unexpected terminator is an invalid token
                let mut k = i + 1;
                loop {
                    match self.ch(k) {
                        None => {
                            if !self.final_ {
                                return Ok(false);
                            }
                            break;
                        }
                        Some(t) if is_name_char(t) => k += 1,
                        Some(t) => {
                            if !matches!(t, '>' | ')' | ',' | '|' | '[' | '%' | ' ' | '\t' | '\n' | '\r') {
                                return Err((E_INVALID_TOKEN, k));
                            }
                            break;
                        }
                    }
                }
            }
            if self.state == State::Epilog {
                return Err((E_JUNK_AFTER_DOC_ELEMENT, i));
            }
            return Err((E_SYNTAX, i));
        }
        let c2 = match self.ch(i + 1) {
            None => return if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(false) },
            Some(c) => c,
        };
        match c2 {
            '?' => self.parse_pi(i),
            '!' => {
                match self.starts_with(i, "<!--") {
                    None => return Ok(false),
                    Some(true) => return self.parse_comment(i),
                    Some(false) => {}
                }
                if self.state == State::Prolog && !self.seen_doctype {
                    match self.starts_with(i, "<!DOCTYPE") {
                        None => return Ok(false),
                        Some(true) => return self.parse_doctype(i),
                        Some(false) => {}
                    }
                }
                if self.state == State::Epilog {
                    return Err((E_JUNK_AFTER_DOC_ELEMENT, i));
                }
                Err((E_SYNTAX, i))
            }
            _ => {
                if self.state == State::Epilog {
                    return Err((E_JUNK_AFTER_DOC_ELEMENT, i));
                }
                if !is_name_start(c2) {
                    return Err((E_INVALID_TOKEN, i + 1));
                }
                self.state = State::Content;
                if !self.seen_doctype && self.use_foreign_dtd && self.param_entity_parsing != 0 {
                    // a foreign DTD is announced like an external subset
                    self.seen_doctype = true;
                    self.dtd.lock().unwrap().has_pe_refs = true;
                    let base = self.base.clone();
                    self.emit_at(i, Ev::ExternalEntityRef(Box::new(ExternalEntityRefEv { context: None, base, sysid: None, pubid: None })));
                }
                self.seen_doctype = true;
                match self.parse_start_tag(i)? {
                    true => Ok(true),
                    false => {
                        self.state = State::Prolog;
                        Ok(false)
                    }
                }
            }
        }
    }

    // -- comments, PIs ------------------------------------------------------

    fn parse_comment(&mut self, i: usize) -> R<bool> {
        let body = i + 4;
        let d = match self.find_str(body, "--") {
            None => return if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(false) },
            Some(d) => d,
        };
        match self.ch(d + 2) {
            None => if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(false) },
            Some('>') => {
                self.check_chars(body, d)?;
                let t = self.text(body, d);
                self.emit_at(i, Ev::Comment(t));
                self.pos = d + 3;
                Ok(true)
            }
            Some(_) => Err((E_INVALID_TOKEN, d)),
        }
    }

    fn parse_pi(&mut self, i: usize) -> R<bool> {
        let ns = i + 2;
        let ne = match self.scan_name(ns)? {
            None => return Ok(false),
            Some(e) => e,
        };
        let target = self.text(ns, ne);
        let end = match self.find_str(ne, "?>") {
            None => return if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(false) },
            Some(e) => e,
        };
        if target.eq_ignore_ascii_case("xml") {
            return Err((E_MISPLACED_XML_PI, i));
        }
        let mut ds = ne;
        if ds < end {
            if !is_ws(self.buf[ds]) {
                return Err((E_INVALID_TOKEN, ds));
            }
            ds = self.skip_ws(ds);
        }
        self.check_chars(ds, end)?;
        let data = self.text(ds.min(end), end);
        self.emit_at(i, Ev::Pi { target, data });
        self.pos = end + 2;
        Ok(true)
    }

    // -- DOCTYPE ------------------------------------------------------------

    fn parse_literal(&self, k: usize) -> R<Option<(String, usize)>> {
        match self.ch(k) {
            None => return Ok(None),
            Some(q) if q == '"' || q == '\'' => {
                let mut e = k + 1;
                while e < self.buf.len() && self.buf[e] != q {
                    e += 1;
                }
                if e >= self.buf.len() {
                    return Ok(None);
                }
                Ok(Some((self.text(k + 1, e), e + 1)))
            }
            Some(_) => Err((E_SYNTAX, k)),
        }
    }

    /// Parse `SYSTEM "x"` / `PUBLIC "p" "x"` at k.  Returns (sysid, pubid,
    /// end) or None (more data needed).  With `notation`, the system literal
    /// after PUBLIC is optional.
    fn parse_external_id(&self, k: usize, notation: bool) -> R<Option<(Option<String>, Option<String>, usize)>> {
        if let Some(true) = self.starts_with(k, "SYSTEM") {
            let s = self.skip_ws(k + 6);
            if s == k + 6 {
                return Err((E_SYNTAX, s));
            }
            return Ok(self.parse_literal(s)?.map(|(lit, e)| (Some(lit), None, e)));
        }
        if let Some(true) = self.starts_with(k, "PUBLIC") {
            let s = self.skip_ws(k + 6);
            if s == k + 6 {
                return Err((E_SYNTAX, s));
            }
            let (pubid, e) = match self.parse_literal(s)? {
                None => return Ok(None),
                Some(x) => x,
            };
            if let Some(bad) = pubid.chars().position(|c| !is_pubid_char(c)) {
                return Err((E_PUBLICID, s + 1 + bad));
            }
            let pubid: String = pubid.split_whitespace().collect::<Vec<_>>().join(" ");
            let s2 = self.skip_ws(e);
            match self.ch(s2) {
                Some('"') | Some('\'') if s2 > e => {
                    return Ok(self.parse_literal(s2)?.map(|(lit, e2)| (Some(lit), Some(pubid.clone()), e2)));
                }
                _ => {
                    if notation {
                        return Ok(Some((None, Some(pubid), e)));
                    }
                    return Err((E_SYNTAX, s2));
                }
            }
        }
        Ok(Some((None, None, k)))
    }

    fn parse_doctype(&mut self, i: usize) -> R<bool> {
        // need the whole header up to '[' or '>'
        let mut k = i + 9;
        let mut quote = None;
        let mut stop = None;
        while k < self.buf.len() {
            let c = self.buf[k];
            match quote {
                Some(q) => {
                    if c == q {
                        quote = None
                    }
                }
                None => {
                    if c == '"' || c == '\'' {
                        quote = Some(c);
                    } else if c == '[' || c == '>' {
                        stop = Some(k);
                        break;
                    }
                }
            }
            k += 1;
        }
        let stop = match stop {
            None => return if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(false) },
            Some(s) => s,
        };
        let s = self.skip_ws(i + 9);
        if s == i + 9 {
            return Err((E_SYNTAX, s));
        }
        let ne = match self.scan_name(s)? {
            None => return Ok(false),
            Some(e) => e,
        };
        let name = self.text(s, ne);
        let mut k = self.skip_ws(ne);
        let mut sysid = None;
        let mut pubid = None;
        if k < stop {
            if k == ne {
                return Err((E_SYNTAX, k));
            }
            match self.parse_external_id(k, false)? {
                None => return Ok(false),
                Some((sy, pu, e)) => {
                    if sy.is_none() {
                        return Err((E_SYNTAX, k));
                    }
                    sysid = sy;
                    pubid = pu;
                    k = self.skip_ws(e);
                }
            }
        }
        if k != stop {
            return Err((E_SYNTAX, k));
        }
        self.seen_doctype = true;
        if sysid.is_some() {
            let standalone = {
                let mut d = self.dtd.lock().unwrap();
                d.has_pe_refs = true;
                d.standalone
            };
            self.doctype_extid = Some((sysid.clone(), pubid.clone()));
            if !standalone {
                self.emit_at(i, Ev::NotStandalone);
            }
        } else if self.use_foreign_dtd {
            self.dtd.lock().unwrap().has_pe_refs = true;
            self.doctype_extid = Some((None, None));
        }
        let internal = self.buf[stop] == '[';
        self.emit_at(i, Ev::StartDoctype(Box::new(StartDoctypeEv { name, sysid, pubid, internal })));
        self.pos = stop + 1;
        if internal {
            self.state = State::Subset;
        } else {
            self.end_doctype(stop);
        }
        Ok(true)
    }

    fn end_doctype(&mut self, at: usize) {
        if let Some((sysid, pubid)) = self.doctype_extid.take() {
            let standalone = self.dtd.lock().unwrap().standalone;
            let parse = self.param_entity_parsing == 2 || (self.param_entity_parsing == 1 && !standalone);
            if parse {
                let base = self.base.clone();
                self.emit_at(at, Ev::ExternalEntityRef(Box::new(ExternalEntityRefEv { context: None, base, sysid, pubid })));
            }
        }
        self.emit_at(at, Ev::EndDoctype);
        self.state = State::Prolog;
    }

    // -- internal and external DTD subset -------------------------------------

    fn step_subset(&mut self) -> R<bool> {
        let i = self.pos;
        if self.state == State::ExtSubset && self.decl_allowed {
            match self.try_decl()? {
                None => return Ok(false),
                Some(_) => return Ok(true),
            }
        }
        let c = self.buf[i];
        if is_ws(c) {
            let k = self.skip_ws(i);
            self.pos = k;
            return Ok(true);
        }
        if c == ']' && self.state == State::Subset {
            let k = self.skip_ws(i + 1);
            match self.ch(k) {
                None => return if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(false) },
                Some('>') => {
                    self.pos = k + 1;
                    self.end_doctype(i);
                    return Ok(true);
                }
                Some(_) => return Err((E_SYNTAX, k)),
            }
        }
        if c == ']' && self.state == State::ExtSubset {
            // end of an INCLUDE section
            match self.starts_with(i, "]]>") {
                None => return Ok(false),
                Some(true) => {
                    self.pos = i + 3;
                    return Ok(true);
                }
                Some(false) => return Err((E_SYNTAX, i)),
            }
        }
        if c == '%' {
            return self.subset_pe_ref(i);
        }
        if c != '<' {
            return Err((if is_xml_char(c) { E_SYNTAX } else { E_INVALID_TOKEN }, i));
        }
        match self.ch(i + 1) {
            None => return if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(false) },
            Some('?') => return self.parse_pi(i),
            Some('!') => {}
            Some(_) => return Err((E_SYNTAX, i)),
        }
        match self.starts_with(i, "<!--") {
            None => return Ok(false),
            Some(true) => return self.parse_comment(i),
            Some(false) => {}
        }
        if self.state == State::ExtSubset {
            if let Some(true) = self.starts_with(i, "<![") {
                return self.conditional_section(i);
            }
        }
        let end = match self.find_tag_end(i) {
            None => return if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(false) },
            Some(e) => e,
        };
        let keep = self.dtd.lock().unwrap().keep_processing;
        for (kw, which) in [("<!ELEMENT", 0), ("<!ATTLIST", 1), ("<!ENTITY", 2), ("<!NOTATION", 3)] {
            if let Some(true) = self.starts_with(i, kw) {
                let k = i + kw.len();
                if !matches!(self.ch(k), Some(c) if is_ws(c)) {
                    return Err((E_SYNTAX, k));
                }
                if keep || which == 0 {
                    match which {
                        0 => self.decl_element(i, k, end)?,
                        1 => self.decl_attlist(i, k, end)?,
                        2 => self.decl_entity(i, k, end)?,
                        _ => self.decl_notation(i, k, end)?,
                    }
                }
                self.pos = end + 1;
                return Ok(true);
            }
        }
        Err((E_SYNTAX, i))
    }

    fn conditional_section(&mut self, i: usize) -> R<bool> {
        let k = self.skip_ws(i + 3);
        let include = match (self.starts_with(k, "INCLUDE"), self.starts_with(k, "IGNORE")) {
            (Some(true), _) => true,
            (_, Some(true)) => false,
            (None, _) | (_, None) => return Ok(false),
            _ => return Err((E_SYNTAX, k)),
        };
        let k2 = self.skip_ws(k + if include { 7 } else { 6 });
        match self.ch(k2) {
            None => return Ok(false),
            Some('[') => {}
            Some(_) => return Err((E_SYNTAX, k2)),
        }
        if include {
            self.pos = k2 + 1;
            return Ok(true);
        }
        // skip a (possibly nested) ignored section
        let mut depth = 1;
        let mut p = k2 + 1;
        while p + 2 < self.buf.len() + 0 {
            if self.buf[p] == '<' && self.buf.get(p + 1) == Some(&'!') && self.buf.get(p + 2) == Some(&'[') {
                depth += 1;
                p += 3;
            } else if self.buf[p] == ']' && self.buf.get(p + 1) == Some(&']') && self.buf.get(p + 2) == Some(&'>') {
                depth -= 1;
                p += 3;
                if depth == 0 {
                    self.pos = p;
                    return Ok(true);
                }
            } else {
                p += 1;
            }
        }
        if self.final_ {
            Err((E_UNCLOSED_TOKEN, i))
        } else {
            Ok(false)
        }
    }

    fn subset_pe_ref(&mut self, i: usize) -> R<bool> {
        let ne = match self.scan_name(i + 1)? {
            None => return Ok(false),
            Some(e) => e,
        };
        match self.ch(ne) {
            None => return if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(false) },
            Some(';') => {}
            Some(_) => return Err((E_INVALID_TOKEN, ne)),
        }
        let name = self.text(i + 1, ne);
        self.pos = ne + 1;
        let (standalone, entity) = {
            let mut d = self.dtd.lock().unwrap();
            d.has_pe_refs = true;
            (d.standalone, d.params.get(&name).cloned())
        };
        let parse = self.param_entity_parsing == 2 || (self.param_entity_parsing == 1 && !standalone);
        if !parse {
            self.dtd.lock().unwrap().keep_processing = standalone;
            if !standalone {
                self.emit_at(i, Ev::NotStandalone);
            }
            return Ok(true);
        }
        match entity {
            None => {
                self.dtd.lock().unwrap().keep_processing = standalone;
                self.emit_at(i, Ev::SkippedEntity { name, is_pe: true });
            }
            Some(ent) => match ent.text {
                Some(text) => {
                    if self.open_entities.contains(&name) {
                        return Err((E_RECURSIVE_ENTITY_REF, i));
                    }
                    self.account(text.len(), i)?;
                    // splice the replacement text into the input (with
                    // surrounding blanks, as the spec requires)
                    let mut ins: Vec<char> = Vec::with_capacity(text.len() + 2);
                    ins.push(' ');
                    ins.extend(text.iter());
                    ins.push(' ');
                    let n = ins.len();
                    let at = self.pos;
                    self.position_at(at);
                    self.buf.splice(at..at, ins);
                    self.widths.splice(at..at, std::iter::repeat(0u8).take(n));
                }
                None => {
                    let base = ent.base.clone();
                    self.emit_at(i, Ev::ExternalEntityRef(Box::new(ExternalEntityRefEv {
                        context: Some(name),
                        base,
                        sysid: ent.sysid.clone(),
                        pubid: ent.pubid.clone(),
                    })));
                }
            },
        }
        Ok(true)
    }

    fn decl_element(&mut self, start: usize, k: usize, end: usize) -> R<()> {
        let s = self.skip_ws(k);
        let ne = self.scan_name(s)?.unwrap_or(end);
        let name = self.text(s, ne);
        let mut p = self.skip_ws(ne);
        if p == ne {
            return Err((E_SYNTAX, p));
        }
        let mut nodes: Vec<ModelNode> = Vec::new();
        if let Some(true) = self.starts_with(p, "EMPTY") {
            nodes.push(ModelNode { ctype: 1, quant: 0, name: None, children: vec![] });
            p += 5;
        } else if let Some(true) = self.starts_with(p, "ANY") {
            nodes.push(ModelNode { ctype: 2, quant: 0, name: None, children: vec![] });
            p += 3;
        } else if self.ch(p) == Some('(') {
            let q = self.skip_ws(p + 1);
            if let Some(true) = self.starts_with(q, "#PCDATA") {
                // Mixed
                let mut names = Vec::new();
                let mut r = self.skip_ws(q + 7);
                while self.ch(r) == Some('|') {
                    r = self.skip_ws(r + 1);
                    let e = self.scan_name(r)?.unwrap_or(end);
                    names.push(self.text(r, e));
                    r = self.skip_ws(e);
                }
                if self.ch(r) != Some(')') {
                    return Err((E_SYNTAX, r));
                }
                r += 1;
                let mut quant = 0;
                if self.ch(r) == Some('*') {
                    quant = 2;
                    r += 1;
                } else if !names.is_empty() {
                    return Err((E_SYNTAX, r));
                }
                nodes.push(ModelNode { ctype: 3, quant, name: None, children: vec![] });
                for n in names {
                    let idx = nodes.len();
                    nodes.push(ModelNode { ctype: 4, quant: 0, name: Some(n), children: vec![] });
                    nodes[0].children.push(idx);
                }
                p = r;
            } else {
                p = self.parse_children_model(p, end, &mut nodes)?;
            }
        } else {
            return Err((E_SYNTAX, p));
        }
        let p = self.skip_ws(p);
        if p != end {
            return Err((E_SYNTAX, p));
        }
        self.emit_at(start, Ev::ElementDecl { name, model: nodes });
        Ok(())
    }

    fn parse_quant(&self, p: &mut usize) -> u8 {
        let q = match self.ch(*p) {
            Some('?') => 1,
            Some('*') => 2,
            Some('+') => 3,
            _ => return 0,
        };
        *p += 1;
        q
    }

    fn parse_children_model(&self, p0: usize, end: usize, nodes: &mut Vec<ModelNode>) -> R<usize> {
        // iterative parse of nested groups; node 0 is the outermost group
        let mut stack: Vec<(usize, Option<char>)> = Vec::new(); // (node index, separator)
        let mut p = p0;
        nodes.push(ModelNode { ctype: 6, quant: 0, name: None, children: vec![] });
        stack.push((0, None));
        p += 1;
        loop {
            if p >= end {
                return Err((E_SYNTAX, p));
            }
            p = self.skip_ws(p);
            // a content particle
            let c = self.ch(p).unwrap_or('>');
            let child;
            if c == '(' {
                child = nodes.len();
                nodes.push(ModelNode { ctype: 6, quant: 0, name: None, children: vec![] });
                let parent = stack.last().unwrap().0;
                nodes[parent].children.push(child);
                stack.push((child, None));
                p += 1;
                continue;
            } else if is_name_start(c) {
                let e = self.scan_name(p)?.unwrap_or(end);
                let name = self.text(p, e);
                p = e;
                let quant = self.parse_quant(&mut p);
                child = nodes.len();
                nodes.push(ModelNode { ctype: 4, quant, name: Some(name), children: vec![] });
                let parent = stack.last().unwrap().0;
                nodes[parent].children.push(child);
            } else {
                return Err((E_SYNTAX, p));
            }
            // after a particle: separator or ')'
            loop {
                p = self.skip_ws(p);
                match self.ch(p) {
                    Some(sep @ ('|' | ',')) => {
                        let top = stack.last_mut().unwrap();
                        match top.1 {
                            None => top.1 = Some(sep),
                            Some(s) if s != sep => return Err((E_SYNTAX, p)),
                            _ => {}
                        }
                        nodes[top.0].ctype = if sep == '|' { 5 } else { 6 };
                        p += 1;
                        break;
                    }
                    Some(')') => {
                        p += 1;
                        let (idx, _) = stack.pop().unwrap();
                        nodes[idx].quant = self.parse_quant(&mut p);
                        if stack.is_empty() {
                            return Ok(p);
                        }
                        continue;
                    }
                    _ => return Err((E_SYNTAX, p)),
                }
            }
        }
    }

    fn decl_attlist(&mut self, start: usize, k: usize, end: usize) -> R<()> {
        let s = self.skip_ws(k);
        let ne = self.scan_name(s)?.unwrap_or(end);
        let elname = self.text(s, ne);
        let mut p = ne;
        loop {
            let q = self.skip_ws(p);
            if q >= end {
                break;
            }
            if q == p {
                return Err((E_SYNTAX, q));
            }
            let ae = self.scan_name(q)?.unwrap_or(end);
            let attname = self.text(q, ae);
            let mut r = self.skip_ws(ae);
            if r == ae {
                return Err((E_SYNTAX, r));
            }
            // type
            let atype;
            if self.ch(r) == Some('(') {
                let close = (r..end).find(|&x| self.buf[x] == ')').ok_or((E_SYNTAX, r))?;
                let inner: String = self.text(r + 1, close);
                let parts: Vec<String> = inner.split('|').map(|x| x.trim().to_string()).collect();
                if parts.iter().any(|x| x.is_empty() || !x.chars().all(is_name_char)) {
                    return Err((E_SYNTAX, r));
                }
                atype = format!("({})", parts.join("|"));
                r = close + 1;
            } else {
                let te = (r..end).find(|&x| is_ws(self.buf[x])).unwrap_or(end);
                let t = self.text(r, te);
                match t.as_str() {
                    "CDATA" | "ID" | "IDREF" | "IDREFS" | "ENTITY" | "ENTITIES" | "NMTOKEN" | "NMTOKENS" => {
                        atype = t;
                        r = te;
                    }
                    "NOTATION" => {
                        let o = self.skip_ws(te);
                        if self.ch(o) != Some('(') {
                            return Err((E_SYNTAX, o));
                        }
                        let close = (o..end).find(|&x| self.buf[x] == ')').ok_or((E_SYNTAX, o))?;
                        let inner: String = self.text(o + 1, close);
                        let parts: Vec<String> = inner.split('|').map(|x| x.trim().to_string()).collect();
                        atype = format!("NOTATION({})", parts.join("|"));
                        r = close + 1;
                    }
                    _ => return Err((E_SYNTAX, r)),
                }
            }
            let d = self.skip_ws(r);
            if d == r {
                return Err((E_SYNTAX, d));
            }
            let (default, required, after);
            if let Some(true) = self.starts_with(d, "#REQUIRED") {
                default = None;
                required = true;
                after = d + 9;
            } else if let Some(true) = self.starts_with(d, "#IMPLIED") {
                default = None;
                required = false;
                after = d + 8;
            } else {
                let mut v = d;
                let mut fixed = false;
                if let Some(true) = self.starts_with(d, "#FIXED") {
                    fixed = true;
                    v = self.skip_ws(d + 6);
                }
                let (lit, e) = self.parse_literal(v)?.ok_or((E_SYNTAX, v))?;
                let lit_chars: Vec<char> = lit.chars().collect();
                let mut value = String::new();
                self.attr_value(&lit_chars, v + 1, &mut value, 0)?;
                if atype != "CDATA" {
                    value = value.split(' ').filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" ");
                }
                default = Some(value);
                required = fixed;
                after = e;
            }
            {
                let mut dtd = self.dtd.lock().unwrap();
                let list = dtd.attlists.entry(elname.clone()).or_default();
                if !list.iter().any(|a| a.name == attname) {
                    list.push(AttDef { name: attname.clone(), is_cdata: atype == "CDATA", default: default.clone() });
                }
            }
            self.emit_at(start, Ev::AttlistDecl(Box::new(AttlistDeclEv {
                elname: elname.clone(),
                attname,
                atype,
                default,
                required,
            })));
            p = after;
        }
        Ok(())
    }

    /// Expand the literal of an entity declaration: character references
    /// and parameter entity references are replaced, general entity
    /// references are kept.
    fn entity_value(&mut self, lit: &[char], at: usize, out: &mut Vec<char>, depth: usize) -> R<()> {
        let mut k = 0;
        while k < lit.len() {
            let c = lit[k];
            if c == '&' {
                if lit.get(k + 1) == Some(&'#') {
                    let (ch, e) = parse_char_ref(lit, k).map_err(|code| (code, at + k))?;
                    out.push(ch);
                    k = e;
                    continue;
                }
                let e = (k + 1..lit.len()).find(|&x| lit[x] == ';').ok_or((E_INVALID_TOKEN, at + k))?;
                if e == k + 1 || !is_name_start(lit[k + 1]) || !lit[k + 1..e].iter().all(|&x| is_name_char(x)) {
                    return Err((E_INVALID_TOKEN, at + k));
                }
                out.extend_from_slice(&lit[k..=e]);
                k = e + 1;
                continue;
            }
            if c == '%' {
                if self.state == State::Subset && depth == 0 {
                    return Err((E_PARAM_ENTITY_REF, at + k));
                }
                let e = (k + 1..lit.len()).find(|&x| lit[x] == ';').ok_or((E_INVALID_TOKEN, at + k))?;
                let name: String = lit[k + 1..e].iter().collect();
                let ent = self.dtd.lock().unwrap().params.get(&name).cloned();
                match ent.and_then(|x| x.text) {
                    Some(text) => {
                        if depth > 40 {
                            return Err((E_RECURSIVE_ENTITY_REF, at + k));
                        }
                        self.account(text.len(), at + k)?;
                        let t: Vec<char> = text.iter().copied().collect();
                        self.entity_value(&t, at, out, depth + 1)?;
                    }
                    None => {}
                }
                k = e + 1;
                continue;
            }
            if !is_xml_char(c) {
                return Err((E_INVALID_TOKEN, at + k));
            }
            out.push(c);
            k += 1;
        }
        Ok(())
    }

    fn decl_entity(&mut self, start: usize, k: usize, end: usize) -> R<()> {
        let mut s = self.skip_ws(k);
        let mut is_pe = false;
        if self.ch(s) == Some('%') {
            let s2 = self.skip_ws(s + 1);
            if s2 == s + 1 {
                return Err((E_SYNTAX, s2));
            }
            is_pe = true;
            s = s2;
        }
        let ne = self.scan_name(s)?.unwrap_or(end);
        let name = self.text(s, ne);
        let p = self.skip_ws(ne);
        if p == ne {
            return Err((E_SYNTAX, p));
        }
        let base = self.base.clone();
        let entity;
        let mut notation = None;
        let mut value_str = None;
        let (sysid, pubid);
        if matches!(self.ch(p), Some('"') | Some('\'')) {
            let (lit, e) = self.parse_literal(p)?.ok_or((E_SYNTAX, p))?;
            let lit_chars: Vec<char> = lit.chars().collect();
            let mut out = Vec::new();
            self.entity_value(&lit_chars, p + 1, &mut out, 0)?;
            let after = self.skip_ws(e);
            if after != end {
                return Err((E_SYNTAX, after));
            }
            value_str = Some(out.iter().collect::<String>());
            sysid = None;
            pubid = None;
            entity = Entity { text: Some(Arc::new(out)), sysid: None, pubid: None, base: None, notation: None };
        } else {
            let (sy, pu, e) = self.parse_external_id(p, false)?.ok_or((E_SYNTAX, p))?;
            if sy.is_none() {
                return Err((E_SYNTAX, p));
            }
            let mut q = self.skip_ws(e);
            if q < end {
                if is_pe || q == e {
                    return Err((E_SYNTAX, q));
                }
                if let Some(true) = self.starts_with(q, "NDATA") {
                    let r = self.skip_ws(q + 5);
                    let re = self.scan_name(r)?.unwrap_or(end);
                    notation = Some(self.text(r, re));
                    q = self.skip_ws(re);
                }
                if q != end {
                    return Err((E_SYNTAX, q));
                }
            }
            sysid = sy;
            pubid = pu;
            entity = Entity {
                text: None,
                sysid: sysid.clone(),
                pubid: pubid.clone(),
                base: base.clone(),
                notation: notation.clone(),
            };
        }
        let stored = name.len() + entity.text.as_ref().map_or(0, |t| 4 * t.len());
        let fresh = {
            let mut dtd = self.dtd.lock().unwrap();
            let map = if is_pe { &mut dtd.params } else { &mut dtd.general };
            if map.contains_key(&name) {
                false
            } else {
                map.insert(name.clone(), entity);
                true
            }
        };
        if fresh && self.track_alloc(stored).is_err() {
            return Err((E_NO_MEMORY, start));
        }
        if fresh {
            let base = if value_str.is_some() { None } else { base };
            self.emit_at(start, Ev::EntityDecl(Box::new(EntityDeclEv { name, is_pe, value: value_str, base, sysid, pubid, notation })));
        }
        Ok(())
    }

    fn decl_notation(&mut self, start: usize, k: usize, end: usize) -> R<()> {
        let s = self.skip_ws(k);
        let ne = self.scan_name(s)?.unwrap_or(end);
        let name = self.text(s, ne);
        let p = self.skip_ws(ne);
        let (sysid, pubid, e) = self.parse_external_id(p, true)?.ok_or((E_SYNTAX, p))?;
        if sysid.is_none() && pubid.is_none() {
            return Err((E_SYNTAX, p));
        }
        let q = self.skip_ws(e);
        if q != end {
            return Err((E_SYNTAX, q));
        }
        let base = self.base.clone();
        self.emit_at(start, Ev::NotationDecl(Box::new(NotationDeclEv { name, base, sysid, pubid })));
        Ok(())
    }

    // -- content ------------------------------------------------------------

    fn step_content(&mut self) -> R<bool> {
        let i = self.pos;
        if self.state == State::ExtContent && self.decl_allowed {
            match self.try_decl()? {
                None => return Ok(false),
                Some(_) => return Ok(true),
            }
        }
        let c = self.buf[i];
        if c == '<' {
            let c2 = match self.ch(i + 1) {
                None => return if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(false) },
                Some(c) => c,
            };
            return match c2 {
                '/' => self.parse_end_tag(i),
                '?' => self.parse_pi(i),
                '!' => {
                    match self.starts_with(i, "<!--") {
                        None => return Ok(false),
                        Some(true) => return self.parse_comment(i),
                        Some(false) => {}
                    }
                    match self.starts_with(i, "<![CDATA[") {
                        None => Ok(false),
                        Some(true) => self.parse_cdata(i),
                        Some(false) => Err((E_INVALID_TOKEN, i)),
                    }
                }
                c2 if is_name_start(c2) => self.parse_start_tag(i),
                _ => Err((E_INVALID_TOKEN, i + 1)),
            };
        }
        if c == '&' {
            return self.content_reference(i);
        }
        if c == '\n' {
            self.emit_at(i, Ev::CharData("\n".to_string()));
            self.pos = i + 1;
            return Ok(true);
        }
        // a run of character data
        let mut k = i;
        let n = self.buf.len();
        // Text converted from another encoding is reported in pieces of at
        // most 1024 bytes of UTF-8 (like Expat's conversion buffer).
        let chunked = matches!(self.enc, Enc::Latin1 | Enc::Table(_) | Enc::Utf16Le | Enc::Utf16Be)
            && self.ev_override.is_none();
        let mut utf8_len = 0usize;
        while k < n {
            let ch = self.buf[k];
            if ch == '<' || ch == '&' || ch == '\n' {
                break;
            }
            if chunked {
                utf8_len += ch.len_utf8();
                if utf8_len > 1024 {
                    break;
                }
            }
            if ch == ']' && k + 2 < n && self.buf[k + 1] == ']' && self.buf[k + 2] == '>' {
                if k > i {
                    break;
                }
                return Err((E_INVALID_TOKEN, k));
            }
            if !is_xml_char(ch) {
                if k > i {
                    break;
                }
                return Err((E_INVALID_TOKEN, k));
            }
            k += 1;
        }
        if k == n && !self.final_ {
            // keep a trailing ']' or ']]' that could start ']]>'
            while k > i && self.buf[k - 1] == ']' && n - k < 2 {
                k -= 1;
            }
            if k == i {
                return Ok(false);
            }
        }
        let t = self.text(i, k);
        self.emit_at(i, Ev::CharData(t));
        self.pos = k;
        Ok(true)
    }

    fn parse_cdata(&mut self, i: usize) -> R<bool> {
        let body = i + 9;
        let end = match self.find_str(body, "]]>") {
            None => {
                return if self.final_ { Err((E_UNCLOSED_CDATA_SECTION, i)) } else { Ok(false) };
            }
            Some(e) => e,
        };
        self.check_chars(body, end)?;
        self.emit_at(i, Ev::StartCdata);
        let mut k = body;
        while k < end {
            if self.buf[k] == '\n' {
                self.emit_at(k, Ev::CharData("\n".to_string()));
                k += 1;
                continue;
            }
            let mut e = k;
            while e < end && self.buf[e] != '\n' {
                e += 1;
            }
            let t = self.text(k, e);
            self.emit_at(k, Ev::CharData(t));
            k = e;
        }
        self.emit_at(end, Ev::EndCdata);
        self.pos = end + 3;
        Ok(true)
    }

    fn content_reference(&mut self, i: usize) -> R<bool> {
        // find ';'
        let mut k = i + 1;
        while k < self.buf.len() && self.buf[k] != ';' && k - i < 1 << 20 {
            let c = self.buf[k];
            if !(is_name_char(c) || c == '#') {
                return Err((E_INVALID_TOKEN, k));
            }
            k += 1;
        }
        if k >= self.buf.len() {
            return if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(false) };
        }
        let raw: Vec<char> = self.buf[i..=k].to_vec();
        self.pos = k + 1;
        self.expand_reference(&raw, i)?;
        Ok(true)
    }

    fn expand_reference(&mut self, raw: &[char], at: usize) -> R<()> {
        if raw.get(1) == Some(&'#') {
            let (c, _) = parse_char_ref(raw, 0).map_err(|code| (code, at))?;
            self.emit_at(at, Ev::CharData(c.to_string()));
            return Ok(());
        }
        let name: String = raw[1..raw.len() - 1].iter().collect();
        if name.is_empty() || !is_name_start(raw[1]) {
            return Err((E_INVALID_TOKEN, at));
        }
        let predefined = match name.as_str() {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => None,
        };
        if let Some(c) = predefined {
            self.emit_at(at, Ev::CharData(c.to_string()));
            return Ok(());
        }
        let (entity, has_pe_refs, standalone) = {
            let d = self.dtd.lock().unwrap();
            (d.general.get(&name).cloned(), d.has_pe_refs, d.standalone)
        };
        let entity = match entity {
            None => {
                if has_pe_refs && !standalone {
                    self.emit_at(at, Ev::SkippedEntity { name, is_pe: false });
                    return Ok(());
                }
                return Err((E_UNDEFINED_ENTITY, at));
            }
            Some(e) => e,
        };
        if entity.notation.is_some() {
            return Err((E_BINARY_ENTITY_REF, at));
        }
        match entity.text {
            None => {
                let base = entity.base.clone();
                self.emit_at(at, Ev::ExternalEntityRef(Box::new(ExternalEntityRefEv {
                    context: Some(name),
                    base,
                    sysid: entity.sysid.clone(),
                    pubid: entity.pubid.clone(),
                })));
                Ok(())
            }
            Some(text) => {
                if self.open_entities.contains(&name) {
                    return Err((E_RECURSIVE_ENTITY_REF, at));
                }
                self.account(text.len(), at)?;
                // parse the replacement text as content, reporting the
                // position of the reference for everything inside it
                let saved_override = self.ev_override;
                if saved_override.is_none() {
                    let p = self.position_at(at);
                    self.ev_override = Some(p);
                }
                self.open_entities.push(name);
                let r = self.parse_entity_content(&text, at);
                self.open_entities.pop();
                self.ev_override = saved_override;
                r
            }
        }
    }

    fn parse_entity_content(&mut self, text: &[char], at: usize) -> R<()> {
        // Swap the replacement text in as the input buffer.
        let saved_buf = std::mem::replace(&mut self.buf, text.to_vec());
        let saved_widths = std::mem::replace(&mut self.widths, vec![0; text.len()]);
        let saved_pos = self.pos;
        let saved_final = self.final_;
        let saved_track = (self.track_idx, self.track_line, self.track_col, self.track_byte);
        let saved_state = self.state;
        self.pos = 0;
        self.final_ = true;
        self.track_idx = 0;
        let depth = self.stack.len();
        if self.state != State::ExtContent {
            self.state = State::Content;
        }
        let mut result = Ok(());
        while self.pos < self.buf.len() {
            match self.step_content() {
                Ok(true) => {}
                Ok(false) => {
                    result = Err((E_UNCLOSED_TOKEN, at));
                    break;
                }
                Err(e) => {
                    result = Err((e.0, at));
                    break;
                }
            }
            if self.stack.len() < depth {
                result = Err((E_ASYNC_ENTITY, at));
                break;
            }
        }
        if result.is_ok() && self.stack.len() != depth {
            result = Err((E_ASYNC_ENTITY, at));
        }
        self.buf = saved_buf;
        self.widths = saved_widths;
        self.pos = saved_pos;
        self.final_ = saved_final;
        self.track_idx = saved_track.0;
        self.track_line = saved_track.1;
        self.track_col = saved_track.2;
        self.track_byte = saved_track.3;
        self.state = saved_state;
        result
    }

    /// Normalise an attribute value (references expanded, whitespace
    /// characters turned into spaces).
    fn attr_value(&mut self, lit: &[char], at: usize, out: &mut String, depth: usize) -> R<()> {
        let mut k = 0;
        while k < lit.len() {
            let c = lit[k];
            match c {
                '<' => return Err((E_INVALID_TOKEN, at + k)),
                '\t' | '\n' | '\r' => {
                    out.push(' ');
                    k += 1;
                }
                '&' => {
                    let e = (k + 1..lit.len()).find(|&x| lit[x] == ';').ok_or((E_INVALID_TOKEN, at + k))?;
                    if lit.get(k + 1) == Some(&'#') {
                        let (ch, _) = parse_char_ref(lit, k).map_err(|code| (code, at + k))?;
                        out.push(ch);
                        k = e + 1;
                        continue;
                    }
                    let name: String = lit[k + 1..e].iter().collect();
                    if name.is_empty() || !is_name_start(lit[k + 1]) || !lit[k + 1..e].iter().all(|&x| is_name_char(x)) {
                        return Err((E_INVALID_TOKEN, at + k));
                    }
                    k = e + 1;
                    let pre = match name.as_str() {
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "amp" => Some('&'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        _ => None,
                    };
                    if let Some(p) = pre {
                        out.push(p);
                        continue;
                    }
                    let (entity, has_pe_refs, standalone) = {
                        let d = self.dtd.lock().unwrap();
                        (d.general.get(&name).cloned(), d.has_pe_refs, d.standalone)
                    };
                    match entity {
                        None => {
                            if has_pe_refs && !standalone {
                                continue;
                            }
                            return Err((E_UNDEFINED_ENTITY, at + k));
                        }
                        Some(ent) => match ent.text {
                            None => return Err((E_ATTRIBUTE_EXTERNAL_ENTITY_REF, at + k)),
                            Some(text) => {
                                if self.open_entities.contains(&name) || depth > 64 {
                                    return Err((E_RECURSIVE_ENTITY_REF, at + k));
                                }
                                self.account(text.len(), at + k)?;
                                self.open_entities.push(name);
                                let t: Vec<char> = text.iter().copied().collect();
                                let r = self.attr_value(&t, at, out, depth + 1);
                                self.open_entities.pop();
                                r?;
                            }
                        },
                    }
                }
                _ => {
                    if !is_xml_char(c) {
                        return Err((E_INVALID_TOKEN, at + k));
                    }
                    out.push(c);
                    k += 1;
                }
            }
        }
        Ok(())
    }

    fn parse_start_tag(&mut self, i: usize) -> R<bool> {
        let end = match self.find_tag_end(i) {
            None => return if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(false) },
            Some(e) => e,
        };
        let ne = self.scan_name(i + 1)?.unwrap_or(end);
        let raw_name = self.text(i + 1, ne);
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut raw_attr_names: Vec<String> = Vec::new();
        let mut p = ne;
        let mut empty = false;
        loop {
            let q = self.skip_ws(p);
            if q == end {
                break;
            }
            if self.buf[q] == '/' {
                if q + 1 == end {
                    empty = true;
                    break;
                }
                return Err((E_INVALID_TOKEN, q));
            }
            if q == p {
                return Err((E_INVALID_TOKEN, q));
            }
            let ae = match self.scan_name(q)? {
                None => return Err((E_INVALID_TOKEN, q)),
                Some(e) => e,
            };
            let aname = self.text(q, ae);
            let mut r = self.skip_ws(ae);
            if self.ch(r) != Some('=') {
                return Err((E_INVALID_TOKEN, r));
            }
            r = self.skip_ws(r + 1);
            let qch = self.ch(r);
            if qch != Some('"') && qch != Some('\'') {
                return Err((E_INVALID_TOKEN, r));
            }
            let qch = qch.unwrap();
            let vs = r + 1;
            let mut ve = vs;
            while ve < end && self.buf[ve] != qch {
                ve += 1;
            }
            let lit: Vec<char> = self.buf[vs..ve].to_vec();
            let mut value = String::new();
            self.attr_value(&lit, vs, &mut value, 0)?;
            if raw_attr_names.contains(&aname) {
                return Err((E_DUPLICATE_ATTRIBUTE, q));
            }
            raw_attr_names.push(aname.clone());
            attrs.push((aname, value));
            p = ve + 1;
        }
        // attribute types and defaults from the DTD
        let nspecified = attrs.len();
        {
            let dtd = self.dtd.lock().unwrap();
            if let Some(defs) = dtd.attlists.get(&raw_name) {
                for def in defs {
                    if let Some(pos) = attrs.iter().position(|(n, _)| *n == def.name) {
                        if !def.is_cdata {
                            let v = attrs[pos].1.split(' ').filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" ");
                            attrs[pos].1 = v;
                        }
                    } else if let Some(d) = &def.default {
                        attrs.push((def.name.clone(), d.clone()));
                    }
                }
            }
        }
        self.pos = end + 1;
        self.start_element(i, raw_name, attrs, nspecified)?;
        if empty {
            let p = self.pos;
            self.end_element(p)?;
        }
        Ok(true)
    }

    fn ns_lookup(&self, prefix: &str) -> Option<String> {
        if prefix == "xml" {
            return Some(XML_NS.to_string());
        }
        self.ns_bindings.get(prefix).and_then(|v| v.last().cloned()).filter(|u| !u.is_empty())
    }

    fn expand(&self, uri: &str, local: &str, prefix: Option<&str>) -> String {
        let sep = self.ns_sep.as_deref().unwrap_or("");
        let mut s = String::with_capacity(uri.len() + local.len() + 2);
        s.push_str(uri);
        s.push_str(sep);
        s.push_str(local);
        if self.ns_triplets {
            if let Some(p) = prefix {
                s.push_str(sep);
                s.push_str(p);
            }
        }
        s
    }

    fn start_element(&mut self, at: usize, raw_name: String, attrs: Vec<(String, String)>, nspecified: usize) -> R<()> {
        if self.ns_sep.is_none() {
            self.emit_at(at, Ev::StartElement { name: raw_name.clone(), attrs, nspecified });
            self.stack.push(Frame { raw_name: raw_name.clone(), name: raw_name, ns_declared: vec![] });
            return Ok(());
        }
        // namespace declarations
        let mut declared = Vec::new();
        let mut rest = Vec::new();
        let mut nspec = 0;
        for (idx, (n, v)) in attrs.into_iter().enumerate() {
            let prefix = if n == "xmlns" {
                Some(String::new())
            } else if let Some(p) = n.strip_prefix("xmlns:") {
                Some(p.to_string())
            } else {
                None
            };
            match prefix {
                Some(p) => {
                    if p == "xmlns" {
                        return Err((E_RESERVED_PREFIX_XMLNS, at));
                    }
                    if p == "xml" && v != XML_NS {
                        return Err((E_RESERVED_PREFIX_XML, at));
                    }
                    if p != "xml" && (v == XML_NS || v == XMLNS_NS) {
                        return Err((E_RESERVED_NAMESPACE_URI, at));
                    }
                    if !p.is_empty() && v.is_empty() {
                        return Err((E_UNDECLARING_PREFIX, at));
                    }
                    let stack = self.ns_bindings.entry(p.clone()).or_default();
                    stack.push(v.clone());
                    let pfx = if p.is_empty() { None } else { Some(p) };
                    let uri = if v.is_empty() { None } else { Some(v) };
                    declared.push((pfx.clone(), uri.clone()));
                }
                None => {
                    if idx < nspecified {
                        nspec += 1;
                    }
                    rest.push((n, v));
                }
            }
        }
        for (pfx, uri) in &declared {
            self.emit_at(at, Ev::StartNs { prefix: pfx.clone(), uri: uri.clone() });
        }
        // element name
        let name = match raw_name.split_once(':') {
            Some((p, local)) => match self.ns_lookup(p) {
                Some(uri) => self.expand(&uri, local, Some(p)),
                None => return Err((E_UNBOUND_PREFIX, at)),
            },
            None => match self.ns_lookup("") {
                Some(uri) => self.expand(&uri, &raw_name, None),
                None => raw_name.clone(),
            },
        };
        let mut out = Vec::with_capacity(rest.len());
        for (n, v) in rest {
            let en = match n.split_once(':') {
                Some((p, local)) => match self.ns_lookup(p) {
                    Some(uri) => self.expand(&uri, local, Some(p)),
                    None => return Err((E_UNBOUND_PREFIX, at)),
                },
                None => n,
            };
            if out.iter().any(|(x, _): &(String, String)| *x == en) {
                return Err((E_DUPLICATE_ATTRIBUTE, at));
            }
            out.push((en, v));
        }
        self.emit_at(at, Ev::StartElement { name: name.clone(), attrs: out, nspecified: nspec });
        let ns_declared = declared.into_iter().map(|(p, _)| (p, None)).collect();
        self.stack.push(Frame { raw_name, name, ns_declared });
        Ok(())
    }

    fn end_element(&mut self, at: usize) -> R<()> {
        let frame = self.stack.pop().unwrap();
        self.emit_at(at, Ev::EndElement { name: frame.name });
        for (prefix, _) in frame.ns_declared.iter().rev() {
            let key = prefix.clone().unwrap_or_default();
            if let Some(v) = self.ns_bindings.get_mut(&key) {
                v.pop();
            }
            self.emit_at(at, Ev::EndNs { prefix: prefix.clone() });
        }
        if self.stack.is_empty() && self.state == State::Content && self.open_entities.is_empty() {
            self.state = State::Epilog;
            self.root_seen = true;
        }
        Ok(())
    }

    fn parse_end_tag(&mut self, i: usize) -> R<bool> {
        let ns = i + 2;
        let ne = match self.scan_name(ns)? {
            None => return Ok(false),
            Some(e) => e,
        };
        let k = self.skip_ws(ne);
        match self.ch(k) {
            None => return if self.final_ { Err((E_UNCLOSED_TOKEN, i)) } else { Ok(false) },
            Some('>') => {}
            Some(_) => return Err((E_INVALID_TOKEN, k)),
        }
        let name = self.text(ns, ne);
        match self.stack.last() {
            Some(f) if f.raw_name == name => {}
            _ => return Err((E_TAG_MISMATCH, ns)),
        }
        self.pos = k + 1;
        self.end_element(i)?;
        Ok(true)
    }
}

/// Parse a character reference `&#...;` starting at lit[k]; returns the
/// character and the index after ';'.
fn parse_char_ref(lit: &[char], k: usize) -> Result<(char, usize), u32> {
    let mut p = k + 2;
    let hex = lit.get(p) == Some(&'x');
    if hex {
        p += 1;
    }
    let start = p;
    let mut v: u64 = 0;
    while p < lit.len() && lit[p] != ';' {
        let d = match lit[p].to_digit(if hex { 16 } else { 10 }) {
            Some(d) => d as u64,
            None => return Err(E_INVALID_TOKEN),
        };
        v = v.saturating_mul(if hex { 16 } else { 10 }).saturating_add(d);
        p += 1;
    }
    if p >= lit.len() || p == start {
        return Err(E_INVALID_TOKEN);
    }
    match char::from_u32(v.min(u32::MAX as u64) as u32) {
        Some(c) if is_xml_char(c) => Ok((c, p + 1)),
        _ => Err(E_BAD_CHAR_REF),
    }
}
