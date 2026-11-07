//! The `_csv` accelerator module (data workstream): dialect objects, the
//! record reader state machine and the record writer used by `Lib/csv.py`.
//!
//! Strings are handled as sequences of code points (`u32`); a parsed field
//! is accumulated in a `Vec<u32>` and turned into a `str` when it ends.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{dict, float, int, list, none, str as pystr, tuple};
use crate::mdefs;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicPtr, Ordering};

const QUOTE_MINIMAL: i64 = 0;
const QUOTE_ALL: i64 = 1;
const QUOTE_NONNUMERIC: i64 = 2;
const QUOTE_NONE: i64 = 3;
const QUOTE_STRINGS: i64 = 4;
const QUOTE_NOTNULL: i64 = 5;

const CR: u32 = '\r' as u32;
const LF: u32 = '\n' as u32;
const SPACE: u32 = ' ' as u32;

static ERROR: crate::subinterp::InterpPtr = crate::subinterp::InterpPtr::new(); // (interp1) per interpreter
static REGISTRY: crate::subinterp::InterpPtr = crate::subinterp::InterpPtr::new(); // (interp1) per interpreter
static FIELD_LIMIT: AtomicIsize = AtomicIsize::new(128 * 1024);

static mut DIALECT_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut READER_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut WRITER_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn dialect_type() -> *mut PyTypeObject {
    &raw mut DIALECT_TYPE
}
fn reader_type() -> *mut PyTypeObject {
    &raw mut READER_TYPE
}
fn writer_type() -> *mut PyTypeObject {
    &raw mut WRITER_TYPE
}

fn error_class() -> &'static PyObject {
    unsafe { &*ERROR.load(Ordering::Acquire) }
}

fn registry() -> &'static PyObject {
    unsafe { &*REGISTRY.load(Ordering::Acquire) }
}

/// Raise `_csv.Error(msg)`.
fn csv_error<T>(msg: String) -> PyResult<T> {
    let exc = ab::call(error_class(), &[&pystr::new_str(&msg)])?;
    set_exc_obj(&exc);
    Err(PyErr)
}

fn set_exc_obj(exc: &PyObject) {
    crate::errors::set_exc(exc.to_ref());
}

// ===================================================================== dialect

#[derive(Clone)]
struct Settings {
    delimiter: u32,
    quotechar: Option<u32>,
    escapechar: Option<u32>,
    lineterminator: Vec<u32>,
    doublequote: bool,
    skipinitialspace: bool,
    strict: bool,
    quoting: i64,
}

#[repr(C)]
struct DialectObj {
    ob_base: PyObject,
    settings: *mut Settings,
    lineterminator: *mut PyObject,
}

fn settings(o: &PyObject) -> &Settings {
    unsafe { &*(*(o.as_ptr() as *mut DialectObj)).settings }
}

const ATTRS: [&str; 8] = [
    "delimiter",
    "doublequote",
    "escapechar",
    "lineterminator",
    "quotechar",
    "quoting",
    "skipinitialspace",
    "strict",
];

fn codepoints(s: &PyObject) -> Vec<u32> {
    let d = pystr::str_data(s);
    (0..d.len()).map(|i| d.get(i)).collect()
}

/// A one-character attribute (`None` allowed when `nullable`).
fn char_setting(name: &str, value: Option<&PyObject>, default: Option<u32>, nullable: bool) -> PyResult<Option<u32>> {
    let Some(v) = value else { return Ok(default) };
    let what = if nullable { "a unicode character or None" } else { "a unicode character" };
    if v.is(none::none()) {
        if nullable {
            return Ok(None);
        }
        raise!(TypeError, "\"{}\" must be {}, not NoneType", name, what);
    }
    if !pystr::is_str(v) {
        raise!(TypeError, "\"{}\" must be {}, not {}", name, what, tname(v));
    }
    let n = pystr::str_len(v);
    if n != 1 {
        raise!(TypeError, "\"{}\" must be {}, not a string of length {}", name, what, n);
    }
    Ok(Some(pystr::str_data(v).get(0)))
}

fn bool_setting(value: Option<&PyObject>, default: bool) -> PyResult<bool> {
    match value {
        Some(v) => ab::is_true(v),
        None => Ok(default),
    }
}

fn build_settings(values: &[Option<PyRef>; 8]) -> PyResult<(Settings, PyRef)> {
    let get = |i: usize| values[i].as_deref();
    let delimiter = char_setting("delimiter", get(0), Some(',' as u32), false)?.unwrap();
    let doublequote = bool_setting(get(1), true)?;
    let escapechar = char_setting("escapechar", get(2), None, true)?;
    let lineterminator = match get(3) {
        None => pystr::new_str("\r\n"),
        Some(v) if pystr::is_str(v) => v.to_ref(),
        Some(v) => raise!(TypeError, "\"lineterminator\" must be a string, not {}", tname(v)),
    };
    let quotechar = char_setting("quotechar", get(4), Some('"' as u32), true)?;
    let quoting = match get(5) {
        None => {
            // an explicit quotechar=None implies that nothing is quoted
            if get(4).is_some_and(|q| q.is(none::none())) {
                QUOTE_NONE
            } else {
                QUOTE_MINIMAL
            }
        }
        Some(v) => {
            if !int::is_int(v) {
                raise!(TypeError, "\"quoting\" must be an integer, not {}", tname(v));
            }
            match int::as_i64(v) {
                Some(q) if (QUOTE_MINIMAL..=QUOTE_NOTNULL).contains(&q) => q,
                _ => raise!(TypeError, "bad \"quoting\" value"),
            }
        }
    };
    let skipinitialspace = bool_setting(get(6), false)?;
    let strict = bool_setting(get(7), false)?;

    if quotechar.is_none() && quoting != QUOTE_NONE {
        raise!(TypeError, "quotechar must be set if quoting enabled");
    }
    let newline = |c: u32| c == CR || c == LF;
    if newline(delimiter) {
        raise!(ValueError, "bad delimiter value");
    }
    if escapechar.is_some_and(newline) {
        raise!(ValueError, "bad escapechar value");
    }
    if quotechar.is_some_and(newline) {
        raise!(ValueError, "bad quotechar value");
    }
    if escapechar == Some(delimiter) {
        raise!(ValueError, "bad delimiter or escapechar value");
    }
    if quotechar == Some(delimiter) {
        raise!(ValueError, "bad delimiter or quotechar value");
    }
    if escapechar.is_some() && escapechar == quotechar {
        raise!(ValueError, "bad escapechar or quotechar value");
    }
    if skipinitialspace && (escapechar == Some(SPACE) || quotechar == Some(SPACE)) {
        raise!(ValueError, "bad escapechar or quotechar value: a space cannot be used with skipinitialspace");
    }
    let term = codepoints(&lineterminator);
    if term.contains(&delimiter) {
        raise!(ValueError, "bad delimiter or lineterminator value");
    }
    if escapechar.is_some_and(|c| term.contains(&c)) {
        raise!(ValueError, "bad escapechar or lineterminator value");
    }
    if quotechar.is_some_and(|c| term.contains(&c)) {
        raise!(ValueError, "bad quotechar or lineterminator value");
    }
    Ok((
        Settings { delimiter, quotechar, escapechar, lineterminator: term, doublequote, skipinitialspace, strict, quoting },
        lineterminator,
    ))
}

fn lookup_dialect(name: &PyObject) -> PyResult {
    match dict::get_item(registry(), name)? {
        Some(d) => Ok(d),
        None => csv_error("unknown dialect".to_string()),
    }
}

/// Build a Dialect of type `tp` from an optional base dialect (object or
/// registered name) and keyword overrides.
fn make_dialect(tp: *mut PyTypeObject, base: Option<&PyObject>, kwargs: &[(&PyObject, &PyObject)]) -> PyResult {
    let mut base = base.filter(|b| !b.is(none::none())).map(|b| b.to_ref());
    if let Some(b) = &base {
        if pystr::is_str(b) {
            base = Some(lookup_dialect(b)?);
        }
    }
    if let Some(b) = &base {
        if kwargs.is_empty() && tp == dialect_type() && b.is_type(dialect_type()) {
            return Ok(b.clone());
        }
    }
    let mut values: [Option<PyRef>; 8] = Default::default();
    for (key, value) in kwargs {
        let name = pystr::to_string_lossy(key);
        match ATTRS.iter().position(|a| *a == name) {
            Some(i) => values[i] = Some(value.to_ref()),
            None => raise!(TypeError, "'{}' is an invalid keyword argument for Dialect()", name),
        }
    }
    // quotechar=None given explicitly (without quoting=) switches quoting
    // off, whatever the base dialect says
    if values[4].as_deref().is_some_and(|q| q.is(none::none())) && values[5].is_none() {
        values[5] = Some(int::new_int(QUOTE_NONE));
    }
    if let Some(b) = &base {
        for (i, attr) in ATTRS.iter().enumerate() {
            if values[i].is_none() {
                values[i] = ab::getattr_opt(b, &pystr::intern_str(attr))?;
            }
        }
    }
    let (s, term) = build_settings(&values)?;
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let d = &mut *(op as *mut DialectObj);
        d.settings = Box::into_raw(Box::new(s));
        d.lineterminator = term.into_raw();
        Ok(PyRef::from_raw(op))
    }
}

fn kw_pairs(kw: *mut PyObject) -> Vec<(PyRef, PyRef)> {
    if kw.is_null() {
        Vec::new()
    } else {
        unsafe { dict::items_vec(&*kw) }
    }
}

unsafe extern "C" fn dialect_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let pos = tuple::tuple_items(&*args);
            let pairs = kw_pairs(kw);
            let mut base: Option<&PyObject> = pos.first().copied();
            let mut rest: Vec<(&PyObject, &PyObject)> = Vec::new();
            for (k, v) in &pairs {
                if pystr::str_eq_ascii(k, "dialect") {
                    if base.is_some() {
                        raise!(TypeError, "Dialect() got multiple values for argument 'dialect'");
                    }
                    base = Some(v);
                } else {
                    rest.push((k, v));
                }
            }
            if pos.len() > 1 {
                raise!(TypeError, "Dialect() takes at most 1 positional argument ({} given)", pos.len());
            }
            make_dialect(tp, base, &rest)
        })())
    }
}

unsafe extern "C" fn dialect_dealloc(o: *mut PyObject) {
    unsafe {
        let d = &mut *(o as *mut DialectObj);
        if !d.settings.is_null() {
            drop(Box::from_raw(d.settings));
            d.settings = std::ptr::null_mut();
        }
        xdecref(std::mem::replace(&mut d.lineterminator, std::ptr::null_mut()));
        generic_free_and_type_decref(o, (*o).type_ptr());
    }
}

fn opt_char(c: Option<u32>) -> PyResult {
    Ok(match c {
        Some(c) => pystr::from_codepoints(&[c]),
        None => none::none_ref(),
    })
}
fn g_delimiter(o: &PyObject) -> PyResult {
    opt_char(Some(settings(o).delimiter))
}
fn g_quotechar(o: &PyObject) -> PyResult {
    opt_char(settings(o).quotechar)
}
fn g_escapechar(o: &PyObject) -> PyResult {
    opt_char(settings(o).escapechar)
}
fn g_lineterminator(o: &PyObject) -> PyResult {
    Ok(unsafe { (*(*(o.as_ptr() as *mut DialectObj)).lineterminator).to_ref() })
}
fn g_doublequote(o: &PyObject) -> PyResult {
    Ok(crate::types::bool_::new_bool(settings(o).doublequote))
}
fn g_skipinitialspace(o: &PyObject) -> PyResult {
    Ok(crate::types::bool_::new_bool(settings(o).skipinitialspace))
}
fn g_strict(o: &PyObject) -> PyResult {
    Ok(crate::types::bool_::new_bool(settings(o).strict))
}
fn g_quoting(o: &PyObject) -> PyResult {
    Ok(int::new_int(settings(o).quoting))
}

fn m_no_pickle(o: &PyObject, _proto: &PyObject) -> PyResult {
    raise!(TypeError, "cannot pickle '{}' object", tname(o))
}

/// Dialect for reader()/writer(): positional dialect (or keyword) plus
/// overrides.
fn dialect_from_call(dialect: Option<&PyObject>, rest: &[(&PyObject, &PyObject)]) -> PyResult {
    make_dialect(dialect_type(), Some(dialect.unwrap_or(&pystr::new_str("excel"))), rest)
}

/// Split FASTKW arguments into positionals, an optional `dialect` keyword
/// and the remaining keywords.
fn split_call<'a>(
    fname: &str,
    args: &[&'a PyObject],
    kwnames: Option<&'a PyObject>,
    max_pos: usize,
) -> PyResult<(Vec<&'a PyObject>, Option<&'a PyObject>, Vec<(&'a PyObject, &'a PyObject)>)> {
    let (pos, kws) = crate::args::split(args, kwnames);
    if pos.len() > max_pos {
        raise!(TypeError, "{}() takes at most {} positional arguments ({} given)", fname, max_pos, pos.len());
    }
    let mut dialect = None;
    let mut rest = Vec::new();
    for (k, v) in kws {
        if pystr::str_eq_ascii(k, "dialect") {
            dialect = Some(v);
        } else {
            rest.push((k, v));
        }
    }
    Ok((pos.to_vec(), dialect, rest))
}

// ===================================================================== reader

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    StartRecord,
    StartField,
    Escaped,
    InField,
    InQuoted,
    EscapeInQuoted,
    QuoteInQuoted,
    EatNewline,
    AfterEscapedNewline,
}

#[repr(C)]
struct ReaderObj {
    ob_base: PyObject,
    input: *mut PyObject,
    dialect: *mut PyObject,
    line_num: isize,
    busy: AtomicBool,
}

fn reader(o: &PyObject) -> &mut ReaderObj {
    unsafe { &mut *(o.as_ptr() as *mut ReaderObj) }
}

/// One record being assembled.
struct Record<'s> {
    s: &'s Settings,
    fields: Vec<PyRef>,
    field: Vec<u32>,
    quoted: bool,
    state: State,
    limit: isize,
}

impl<'s> Record<'s> {
    fn add(&mut self, c: u32) -> PyResult<()> {
        if self.field.len() as isize >= self.limit {
            return csv_error(format!("field larger than field limit ({})", self.limit));
        }
        self.field.push(c);
        Ok(())
    }

    fn save(&mut self) -> PyResult<()> {
        let text = std::mem::take(&mut self.field);
        let quoted = std::mem::replace(&mut self.quoted, false);
        let q = self.s.quoting;
        let value = if !quoted && text.is_empty() && (q == QUOTE_NOTNULL || q == QUOTE_STRINGS) {
            none::none_ref()
        } else {
            let s = pystr::from_codepoints(&text);
            if !quoted && !text.is_empty() && (q == QUOTE_NONNUMERIC || q == QUOTE_STRINGS) {
                ab::call(type_obj(float::float_type()), &[&s])?
            } else {
                s
            }
        };
        self.fields.push(value);
        Ok(())
    }

    fn is_quote(&self, c: u32) -> bool {
        self.s.quoting != QUOTE_NONE && Some(c) == self.s.quotechar
    }

    /// Feed one character; `None` marks the end of an input line.
    fn feed(&mut self, c: Option<u32>) -> PyResult<()> {
        let s = self.s;
        let newline = |c: Option<u32>| matches!(c, Some(CR) | Some(LF));
        loop {
            match self.state {
                State::StartRecord => {
                    match c {
                        None => return Ok(()), // empty line: empty record
                        Some(CR) | Some(LF) => {
                            self.state = State::EatNewline;
                            return Ok(());
                        }
                        _ => {
                            self.state = State::StartField;
                            continue;
                        }
                    }
                }
                State::StartField => {
                    if c.is_none() || newline(c) {
                        self.save()?;
                        self.state = if c.is_none() { State::StartRecord } else { State::EatNewline };
                    } else {
                        let ch = c.unwrap();
                        if self.is_quote(ch) {
                            self.quoted = true;
                            self.state = State::InQuoted;
                        } else if Some(ch) == s.escapechar {
                            self.state = State::Escaped;
                        } else if ch == SPACE && s.skipinitialspace {
                        } else if ch == s.delimiter {
                            self.save()?;
                        } else {
                            self.add(ch)?;
                            self.state = State::InField;
                        }
                    }
                }
                State::Escaped => {
                    match c {
                        Some(ch) if ch == CR || ch == LF => {
                            self.add(ch)?;
                            self.state = State::AfterEscapedNewline;
                        }
                        None => {
                            self.add(LF)?;
                            self.state = State::InField;
                        }
                        Some(ch) => {
                            self.add(ch)?;
                            self.state = State::InField;
                        }
                    }
                }
                State::AfterEscapedNewline => {
                    if c.is_none() {
                        return Ok(());
                    }
                    self.state = State::InField;
                    continue;
                }
                State::InField => {
                    if c.is_none() || newline(c) {
                        self.save()?;
                        self.state = if c.is_none() { State::StartRecord } else { State::EatNewline };
                    } else {
                        let ch = c.unwrap();
                        if Some(ch) == s.escapechar {
                            self.state = State::Escaped;
                        } else if ch == s.delimiter {
                            self.save()?;
                            self.state = State::StartField;
                        } else {
                            self.add(ch)?;
                        }
                    }
                }
                State::InQuoted => {
                    if let Some(ch) = c {
                        if Some(ch) == s.escapechar {
                            self.state = State::EscapeInQuoted;
                        } else if self.is_quote(ch) {
                            self.state = if s.doublequote { State::QuoteInQuoted } else { State::InField };
                        } else {
                            self.add(ch)?;
                        }
                    }
                    // end of line inside quotes: the record continues
                }
                State::EscapeInQuoted => {
                    self.add(c.unwrap_or(LF))?;
                    self.state = State::InQuoted;
                }
                State::QuoteInQuoted => {
                    if c.is_some_and(|ch| self.is_quote(ch)) {
                        self.add(c.unwrap())?;
                        self.state = State::InQuoted;
                    } else if c == Some(s.delimiter) {
                        self.save()?;
                        self.state = State::StartField;
                    } else if c.is_none() || newline(c) {
                        self.save()?;
                        self.state = if c.is_none() { State::StartRecord } else { State::EatNewline };
                    } else if !s.strict {
                        self.add(c.unwrap())?;
                        self.state = State::InField;
                    } else {
                        let d = char::from_u32(s.delimiter).unwrap_or('?');
                        let q = s.quotechar.and_then(char::from_u32).unwrap_or('?');
                        return csv_error(format!("'{}' expected after '{}'", d, q));
                    }
                }
                State::EatNewline => match c {
                    Some(CR) | Some(LF) => {}
                    None => self.state = State::StartRecord,
                    Some(_) => {
                        return csv_error(
                            "new-line character seen in unquoted field - do you need to open the file with newline=''?"
                                .to_string(),
                        );
                    }
                },
            }
            return Ok(());
        }
    }
}

fn read_record(o: &PyObject) -> PyResult<Option<PyRef>> {
    let r = reader(o);
    let dialect = unsafe { (*r.dialect).to_ref() };
    let s = settings(&dialect).clone();
    let input = unsafe { (*r.input).to_ref() };
    let mut rec = Record {
        s: &s,
        fields: Vec::new(),
        field: Vec::new(),
        quoted: false,
        state: State::StartRecord,
        limit: FIELD_LIMIT.load(Ordering::Relaxed),
    };
    loop {
        let line = match ab::iter_next(&input)? {
            Some(l) => l,
            None => {
                let pending = rec.state != State::StartRecord
                    && rec.state != State::EatNewline
                    && (!rec.field.is_empty() || rec.state == State::InQuoted);
                if !pending {
                    return Ok(None);
                }
                if s.strict {
                    return csv_error("unexpected end of data".to_string());
                }
                rec.save()?;
                break;
            }
        };
        if !pystr::is_str(&line) {
            return csv_error(format!(
                "iterator should return strings, not {} (the file should be opened in text mode)",
                tname(&line)
            ));
        }
        r.line_num += 1;
        let d = pystr::str_data(&line);
        for i in 0..d.len() {
            rec.feed(Some(d.get(i)))?;
        }
        rec.feed(None)?;
        if rec.state == State::StartRecord {
            break;
        }
    }
    Ok(Some(list::new_list(rec.fields)))
}

unsafe extern "C" fn reader_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let obj = &*o;
        let r = reader(obj);
        if r.busy.swap(true, Ordering::AcqRel) {
            let _: PyResult<()> = csv_error("reader re-entered while reading".to_string());
            return std::ptr::null_mut();
        }
        let result = read_record(obj);
        reader(obj).busy.store(false, Ordering::Release);
        match result {
            Ok(Some(row)) => row.into_raw(),
            Ok(None) => std::ptr::null_mut(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}

unsafe extern "C" fn iter_self(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

unsafe extern "C" fn reader_dealloc(o: *mut PyObject) {
    unsafe {
        let r = &mut *(o as *mut ReaderObj);
        xdecref(std::mem::replace(&mut r.input, std::ptr::null_mut()));
        xdecref(std::mem::replace(&mut r.dialect, std::ptr::null_mut()));
        generic_free_and_type_decref(o, (*o).type_ptr());
    }
}

fn g_reader_dialect(o: &PyObject) -> PyResult {
    Ok(unsafe { (*reader(o).dialect).to_ref() })
}
fn g_line_num(o: &PyObject) -> PyResult {
    Ok(int::new_int(reader(o).line_num as i64))
}

fn m_reader(_m: &PyObject, args: &[&PyObject], kwnames: Option<&PyObject>) -> PyResult {
    let (pos, dialect, rest) = split_call("reader", args, kwnames, 2)?;
    let Some(source) = pos.first() else {
        raise!(TypeError, "expected at least 1 argument, got 0");
    };
    let input = ab::get_iter(source)?;
    let dialect = dialect_from_call(pos.get(1).copied().or(dialect), &rest)?;
    unsafe {
        let tp = reader_type();
        let op = generic_alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let r = &mut *(op as *mut ReaderObj);
        r.input = input.into_raw();
        r.dialect = dialect.into_raw();
        r.line_num = 0;
        std::ptr::write(&mut r.busy, AtomicBool::new(false));
        Ok(PyRef::from_raw(op))
    }
}

// ===================================================================== writer

#[repr(C)]
struct WriterObj {
    ob_base: PyObject,
    write: *mut PyObject,
    dialect: *mut PyObject,
}

fn writer(o: &PyObject) -> &mut WriterObj {
    unsafe { &mut *(o.as_ptr() as *mut WriterObj) }
}

fn is_number(o: &PyObject) -> bool {
    unsafe {
        let nb = (*o.type_ptr()).tp_as_number;
        if nb.is_null() {
            return false;
        }
        (*nb).nb_index.is_some() || (*nb).nb_int.is_some() || (*nb).nb_float.is_some()
            || crate::types::complex::is_complex(o)
    }
}

/// Append one field to `out`, quoting and escaping as the dialect asks.
fn emit_field(s: &Settings, out: &mut Vec<u32>, text: &[u32], mut quoted: bool, must_protect: bool) -> PyResult<()> {
    let mut body: Vec<u32> = Vec::with_capacity(text.len() + 2);
    for &c in text {
        let special = c == s.delimiter
            || Some(c) == s.escapechar
            || Some(c) == s.quotechar
            || c == CR
            || c == LF
            || s.lineterminator.contains(&c);
        let mut escape = false;
        if special {
            if s.quoting == QUOTE_NONE {
                escape = true;
            } else if Some(c) == s.quotechar {
                if s.doublequote {
                    body.push(c);
                    quoted = true;
                } else {
                    escape = true;
                }
            } else if Some(c) == s.escapechar {
                escape = true;
            } else {
                quoted = true;
            }
            if escape {
                match s.escapechar {
                    Some(e) => body.push(e),
                    None => return csv_error("need to escape, but no escapechar set".to_string()),
                }
            }
        }
        body.push(c);
    }
    if must_protect && !quoted {
        if s.quoting == QUOTE_MINIMAL {
            quoted = true;
        } else if text.is_empty() {
            return csv_error("empty field must be quoted to be read back (single-field record, or space delimiter with skipinitialspace)".to_string());
        }
    }
    if quoted {
        let q = s.quotechar.unwrap_or('"' as u32);
        out.push(q);
        out.extend_from_slice(&body);
        out.push(q);
    } else {
        out.extend_from_slice(&body);
    }
    Ok(())
}

fn m_writerow(o: &PyObject, row: &PyObject) -> PyResult {
    let w = writer(o);
    let dialect = unsafe { (*w.dialect).to_ref() };
    let s = settings(&dialect).clone();
    let iter = match ab::get_iter(row) {
        Ok(it) => it,
        Err(e) => {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                return csv_error(format!("iterable expected, not {}", tname(row)));
            }
            return Err(e);
        }
    };
    let mut items: Vec<PyRef> = Vec::new();
    while let Some(item) = ab::iter_next(&iter)? {
        items.push(item);
    }
    let mut out: Vec<u32> = Vec::new();
    let single = items.len() == 1;
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(s.delimiter);
        }
        let is_none = item.is(none::none());
        let quoted = match s.quoting {
            QUOTE_NONNUMERIC => !is_number(item),
            QUOTE_ALL => true,
            QUOTE_STRINGS => pystr::is_str(item),
            QUOTE_NOTNULL => !is_none,
            _ => false,
        };
        let text = if is_none {
            Vec::new()
        } else if pystr::is_str(item) {
            codepoints(item)
        } else {
            {
                let text = ab::str(item)?;
                codepoints(&text)
            }
        };
        let protect = text.is_empty() && (single || (s.delimiter == SPACE && s.skipinitialspace));
        emit_field(&s, &mut out, &text, quoted, protect)?;
    }
    out.extend_from_slice(&s.lineterminator);
    let line = pystr::from_codepoints(&out);
    let write = unsafe { (*w.write).to_ref() };
    ab::call(&write, &[&line])
}

fn m_writerows(o: &PyObject, rows: &PyObject) -> PyResult {
    let iter = ab::get_iter(rows)?;
    while let Some(row) = ab::iter_next(&iter)? {
        m_writerow(o, &row)?;
    }
    Ok(none::none_ref())
}

unsafe extern "C" fn writer_dealloc(o: *mut PyObject) {
    unsafe {
        let w = &mut *(o as *mut WriterObj);
        xdecref(std::mem::replace(&mut w.write, std::ptr::null_mut()));
        xdecref(std::mem::replace(&mut w.dialect, std::ptr::null_mut()));
        generic_free_and_type_decref(o, (*o).type_ptr());
    }
}

fn g_writer_dialect(o: &PyObject) -> PyResult {
    Ok(unsafe { (*writer(o).dialect).to_ref() })
}

fn m_writer(_m: &PyObject, args: &[&PyObject], kwnames: Option<&PyObject>) -> PyResult {
    let (pos, dialect, rest) = split_call("writer", args, kwnames, 2)?;
    let Some(target) = pos.first() else {
        raise!(TypeError, "expected at least 1 argument, got 0");
    };
    let write = match ab::getattr_opt(target, &pystr::intern_str("write"))? {
        Some(w) => w,
        None => raise!(TypeError, "argument 1 must have a \"write\" method"),
    };
    let dialect = dialect_from_call(pos.get(1).copied().or(dialect), &rest)?;
    unsafe {
        let tp = writer_type();
        let op = generic_alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let w = &mut *(op as *mut WriterObj);
        w.write = write.into_raw();
        w.dialect = dialect.into_raw();
        Ok(PyRef::from_raw(op))
    }
}

unsafe extern "C" fn no_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    let name = unsafe { std::ffi::CStr::from_ptr((*tp).tp_name).to_string_lossy().into_owned() };
    into_c(Err(err!(TypeError, "cannot create '{}' instances", name)))
}

// ===================================================================== registry

fn m_register_dialect(_m: &PyObject, args: &[&PyObject], kwnames: Option<&PyObject>) -> PyResult {
    let (pos, dialect, rest) = split_call("register_dialect", args, kwnames, 2)?;
    let Some(name) = pos.first() else {
        raise!(TypeError, "register_dialect() expected at least 1 argument, got 0");
    };
    if !pystr::is_str(name) {
        raise!(TypeError, "dialect name must be a string");
    }
    let base = pos.get(1).copied().or(dialect);
    let d = make_dialect(dialect_type(), base, &rest)?;
    dict::set_item(registry(), name, &d)?;
    Ok(none::none_ref())
}

fn m_unregister_dialect(_m: &PyObject, name: &PyObject) -> PyResult {
    if dict::del_item(registry(), name)? {
        Ok(none::none_ref())
    } else {
        csv_error("unknown dialect".to_string())
    }
}

fn m_get_dialect(_m: &PyObject, name: &PyObject) -> PyResult {
    lookup_dialect(name)
}

fn m_list_dialects(_m: &PyObject) -> PyResult {
    Ok(list::new_list(dict::keys_vec(registry())))
}

fn m_field_size_limit(_m: &PyObject, args: &[&PyObject], kwnames: Option<&PyObject>) -> PyResult {
    let p = crate::args::Params::new("field_size_limit", &["new_limit"], 0, 0);
    let [limit] = crate::args::parse::<1>(&p, args, kwnames)?;
    let old = FIELD_LIMIT.load(Ordering::Relaxed);
    if let Some(l) = limit {
        if !int::is_int(l) {
            raise!(TypeError, "limit must be an integer");
        }
        let v = ab::index_as_ssize(l, None)?;
        FIELD_LIMIT.store(v, Ordering::Relaxed);
    }
    Ok(int::new_int(old as i64))
}

// ===================================================================== module

static INIT: std::sync::Once = std::sync::Once::new();

const MODULE_DOC: &str = "Record reader, record writer and dialect registry behind the csv module.\n";

pub fn init_module() -> PyResult {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        res = (|| -> PyResult<()> {
            let t = &mut *dialect_type();
            t.tp_name = c"_csv.Dialect".as_ptr();
            t.tp_basicsize = std::mem::size_of::<DialectObj>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
            t.tp_new = Some(dialect_new);
            t.tp_dealloc = Some(dialect_dealloc);
            t.tp_alloc = Some(generic_alloc);
            t.tp_free = Some(generic_free);
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_doc = c"Dialect(dialect=None, **options)\n\nImmutable set of options describing one flavour of CSV.".as_ptr();
            t.tp_getset = crate::getset_table![
                ("delimiter", g_delimiter),
                ("doublequote", g_doublequote),
                ("escapechar", g_escapechar),
                ("lineterminator", g_lineterminator),
                ("quotechar", g_quotechar),
                ("quoting", g_quoting),
                ("skipinitialspace", g_skipinitialspace),
                ("strict", g_strict),
            ];
            t.tp_methods = mdefs![("__reduce_ex__", O, m_no_pickle, "Dialects cannot be pickled.")];
            type_ready(dialect_type())?;

            let t = &mut *reader_type();
            t.tp_name = c"_csv.reader".as_ptr();
            t.tp_basicsize = std::mem::size_of::<ReaderObj>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_DISALLOW_INSTANTIATION;
            t.tp_new = Some(no_new);
            t.tp_dealloc = Some(reader_dealloc);
            t.tp_iter = Some(iter_self);
            t.tp_iternext = Some(reader_next);
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_doc = c"Iterator over the records of CSV input; see csv.reader().".as_ptr();
            t.tp_getset = crate::getset_table![("dialect", g_reader_dialect), ("line_num", g_line_num)];
            type_ready(reader_type())?;

            let t = &mut *writer_type();
            t.tp_name = c"_csv.writer".as_ptr();
            t.tp_basicsize = std::mem::size_of::<WriterObj>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_DISALLOW_INSTANTIATION;
            t.tp_new = Some(no_new);
            t.tp_dealloc = Some(writer_dealloc);
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_doc = c"Formats rows as CSV records and passes them to a write() method; see csv.writer().".as_ptr();
            t.tp_getset = crate::getset_table![("dialect", g_writer_dialect)];
            t.tp_methods = mdefs![
                ("writerow", O, m_writerow, "writerow($self, row, /)\n--\n\nWrite one record built from the items of *row* (converted with str(), None as an empty field)."),
                ("writerows", O, m_writerows, "writerows($self, rows, /)\n--\n\nWrite one record for every row in the iterable *rows*."),
            ];
            type_ready(writer_type())?;

            let ns = dict::new_dict();
            dict::set_item_str(&ns, "__module__", &pystr::new_str("_csv"))?;
            let err = ab::call(
                type_obj(type_type()),
                &[&pystr::new_str("Error"), &tuple::new_tuple(vec![type_ref(crate::types::exceptions::Exception())]), &ns],
            )?;
            ERROR.store(err.into_raw(), Ordering::Release);
            REGISTRY.store(dict::new_dict().into_raw(), Ordering::Release);
            Ok(())
        })();
    });
    res?;
    let m = crate::types::module::new_module("_csv")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str(MODULE_DOC))?;
    dict::set_item_str(&d, "__version__", &pystr::new_str("1.0"))?;
    for (name, v) in [
        ("QUOTE_MINIMAL", QUOTE_MINIMAL),
        ("QUOTE_ALL", QUOTE_ALL),
        ("QUOTE_NONNUMERIC", QUOTE_NONNUMERIC),
        ("QUOTE_NONE", QUOTE_NONE),
        ("QUOTE_STRINGS", QUOTE_STRINGS),
        ("QUOTE_NOTNULL", QUOTE_NOTNULL),
    ] {
        dict::set_item_str(&d, name, &int::new_int(v))?;
    }
    dict::set_item_str(&d, "Dialect", &type_ref(dialect_type()))?;
    dict::set_item_str(&d, "Reader", &type_ref(reader_type()))?;
    dict::set_item_str(&d, "Writer", &type_ref(writer_type()))?;
    dict::set_item_str(&d, "Error", error_class())?;
    dict::set_item_str(&d, "_dialects", registry())?;
    crate::builtins::add_methods_to_dict(
        &d,
        mdefs![
            ("reader", FASTKW, m_reader, "reader(iterable, dialect='excel', **options)\n--\n\nIterate over the CSV records of *iterable*, which yields lines as str;\neach record is returned as a list of fields."),
            ("writer", FASTKW, m_writer, "writer(fileobj, dialect='excel', **options)\n--\n\nReturn an object whose writerow()/writerows() write CSV records to\n*fileobj* (anything with a write() method)."),
            ("register_dialect", FASTKW, m_register_dialect, "register_dialect(name, dialect=None, **options)\n--\n\nRegister a dialect under *name*."),
            ("unregister_dialect", O, m_unregister_dialect, "unregister_dialect(name, /)\n--\n\nForget the dialect registered under *name*."),
            ("get_dialect", O, m_get_dialect, "get_dialect(name, /)\n--\n\nReturn the dialect registered under *name*."),
            ("list_dialects", NOARGS, m_list_dialects, "list_dialects($module, /)\n--\n\nNames of all registered dialects."),
            ("field_size_limit", FASTKW, m_field_size_limit, "field_size_limit($module, /, new_limit=<unrepresentable>)\n--\n\nReturn the maximum field size accepted by readers, optionally setting a\nnew one."),
        ],
        Some(&m),
    )?;
    Ok(m)
}
