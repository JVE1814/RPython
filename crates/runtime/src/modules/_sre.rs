//! `_sre`: the regular expression accelerator module (re.Pattern, re.Match,
//! the scanner and template helpers) over the `rpy-sre` engine.
//!
//! Unlike CPython, patterns are parsed and compiled in Rust: `_sre.compile`
//! keeps CPython's signature `(pattern, flags, code, groups, groupindex,
//! indexgroup)` but only validates `code` and compiles `pattern` itself.

use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::modules::text_util::{self as tu, ByteData, StrBuf};
use crate::object::*;
use crate::types::str::{self as pystr, StrData};
use crate::types::{bool_, bytes, dict, int, list, none, tuple};
use crate::types::typeobject::{tname, type_ready};

use rpy_sre::{Captures, MatchMode, Scratch, TemplateItem, UnicodeProvider};
use std::collections::HashMap;
use std::ffi::c_int;
use std::sync::{Mutex, OnceLock};

// =====================================================================================
// Unicode provider built from rpy-unicode (exact CPython semantics)
// =====================================================================================

fn p_is_word(c: u32) -> bool {
    c == b'_' as u32 || rpy_unicode::is_alnum(c)
}
fn p_is_digit(c: u32) -> bool {
    rpy_unicode::is_decimal(c)
}
fn p_is_space(c: u32) -> bool {
    rpy_unicode::is_space(c)
}
fn p_lower(c: u32) -> u32 {
    rpy_unicode::py_to_lowercase(c)
}
fn p_upper(c: u32) -> u32 {
    rpy_unicode::py_to_uppercase(c)
}
fn p_id_start(c: u32) -> bool {
    c == b'_' as u32 || rpy_unicode::is_xid_start(c)
}
fn p_id_continue(c: u32) -> bool {
    rpy_unicode::is_xid_continue(c)
}
fn p_is_alpha(c: u32) -> bool {
    rpy_unicode::is_alpha(c)
}
fn p_is_printable(c: u32) -> bool {
    rpy_unicode::is_printable(c)
}
fn p_lookup_name(n: &str) -> Option<u32> {
    rpy_unicode::lookup_name(n)
}
fn p_locale_lower(b: u8) -> u8 {
    unsafe { libc::tolower(b as c_int) as u8 }
}
fn p_locale_upper(b: u8) -> u8 {
    unsafe { libc::toupper(b as c_int) as u8 }
}
fn p_locale_is_alnum(b: u8) -> bool {
    unsafe { libc::isalnum(b as c_int) != 0 }
}

struct CaseTables {
    cased: Vec<u32>,
    class_of: Vec<u32>,
    classes: Vec<&'static [u32]>,
}

fn case_tables() -> &'static CaseTables {
    static T: OnceLock<CaseTables> = OnceLock::new();
    T.get_or_init(build_case_tables)
}

fn build_case_tables() -> CaseTables {
    let mut inverse: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut by_upper: HashMap<Vec<u32>, Vec<u32>> = HashMap::new();
    let mut buf = [0u32; 3];
    for cp in 0u32..0x20000 {
        if matches!(cp, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7A3 | 0xD800..=0xF8FF) {
            continue;
        }
        let l0 = rpy_unicode::py_to_lowercase(cp);
        let nl = rpy_unicode::to_lower_full(cp, &mut buf);
        let lo_single = nl <= 1;
        let nu = rpy_unicode::to_upper_full(cp, &mut buf);
        let up: Vec<u32> = buf[..nu].to_vec();
        let up_is_self = nu == 1 && up[0] == cp;
        if l0 == cp && up_is_self {
            continue;
        }
        if l0 != cp {
            inverse.entry(l0).or_default().push(cp);
        }
        if lo_single {
            let e = by_upper.entry(up).or_default();
            if !e.contains(&l0) {
                e.push(l0);
            }
        }
    }
    let mut parent: HashMap<u32, u32> = HashMap::new();
    fn find(parent: &mut HashMap<u32, u32>, x: u32) -> u32 {
        let mut r = x;
        while let Some(&p) = parent.get(&r) {
            if p == r {
                break;
            }
            r = p;
        }
        let mut y = x;
        while let Some(&p) = parent.get(&y) {
            if p == y {
                break;
            }
            parent.insert(y, r);
            y = p;
        }
        r
    }
    for lows in by_upper.values() {
        if lows.len() < 2 {
            continue;
        }
        let first = lows[0];
        parent.entry(first).or_insert(first);
        for &l in &lows[1..] {
            parent.entry(l).or_insert(l);
            let a = find(&mut parent, first);
            let b = find(&mut parent, l);
            if a != b {
                parent.insert(b, a);
            }
        }
    }
    let mut members: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut all_lowers: Vec<u32> = inverse.keys().copied().collect();
    all_lowers.extend(parent.keys().copied());
    all_lowers.sort_unstable();
    all_lowers.dedup();
    for l in all_lowers {
        let rep = if parent.contains_key(&l) { find(&mut parent, l) } else { l };
        let m = members.entry(rep).or_default();
        m.push(l);
        if let Some(inv) = inverse.get(&l) {
            m.extend_from_slice(inv);
        }
    }
    let mut classes: Vec<&'static [u32]> = Vec::new();
    let mut pairs: Vec<(u32, u32)> = Vec::new();
    for (_, mut m) in members {
        m.sort_unstable();
        m.dedup();
        if m.len() < 2 {
            continue;
        }
        let idx = classes.len() as u32;
        for &c in &m {
            pairs.push((c, idx));
        }
        classes.push(Box::leak(m.into_boxed_slice()));
    }
    pairs.sort_unstable();
    CaseTables { cased: pairs.iter().map(|p| p.0).collect(), class_of: pairs.iter().map(|p| p.1).collect(), classes }
}

fn p_case_class(c: u32) -> &'static [u32] {
    let t = case_tables();
    match t.cased.binary_search(&c) {
        Ok(i) => t.classes[t.class_of[i] as usize],
        Err(_) => &[],
    }
}
fn p_cased_chars() -> &'static [u32] {
    &case_tables().cased
}

pub static PROVIDER: UnicodeProvider = UnicodeProvider {
    is_word: p_is_word,
    is_digit: p_is_digit,
    is_space: p_is_space,
    lower: p_lower,
    upper: p_upper,
    case_class: p_case_class,
    cased_chars: p_cased_chars,
    is_identifier_start: p_id_start,
    is_identifier_continue: p_id_continue,
    is_alpha: p_is_alpha,
    is_printable: p_is_printable,
    lookup_name: p_lookup_name,
    locale_lower: p_locale_lower,
    locale_upper: p_locale_upper,
    locale_is_alnum: p_locale_is_alnum,
};

// =====================================================================================
// Subjects
// =====================================================================================

#[derive(Clone, Copy)]
enum Units<'a> {
    B(&'a [u8]),
    W(&'a [u16]),
    D(&'a [u32]),
}

impl Units<'_> {
    fn len(&self) -> usize {
        match self {
            Units::B(s) => s.len(),
            Units::W(s) => s.len(),
            Units::D(s) => s.len(),
        }
    }
    fn get(&self, i: usize) -> u32 {
        match self {
            Units::B(s) => s[i] as u32,
            Units::W(s) => s[i] as u32,
            Units::D(s) => s[i],
        }
    }
}

macro_rules! dispatch {
    ($u:expr, $s:ident => $body:expr) => {
        match $u {
            Units::B($s) => $body,
            Units::W($s) => $body,
            Units::D($s) => $body,
        }
    };
}

enum Subject<'a> {
    Str(&'a PyObject),
    Bytes(ByteData<'a>),
}

impl Subject<'_> {
    fn units(&self) -> Units<'_> {
        match self {
            Subject::Str(o) => match pystr::str_data(o) {
                StrData::L1(s) => Units::B(s),
                StrData::U2(s) => Units::W(s),
                StrData::U4(s) => Units::D(s),
            },
            Subject::Bytes(b) => Units::B(b.as_slice()),
        }
    }
    fn is_bytes(&self) -> bool {
        matches!(self, Subject::Bytes(_))
    }
}

fn get_subject<'a>(o: &'a PyObject) -> PyResult<Subject<'a>> {
    if pystr::is_str(o) {
        return Ok(Subject::Str(o));
    }
    match tu::byte_data(o) {
        Some(b) => Ok(Subject::Bytes(b)),
        None => raise!(TypeError, "expected string or bytes-like object, got '{}'", tname(o)),
    }
}

fn subject_for<'a>(pat: &PatternObj, o: &'a PyObject) -> PyResult<Subject<'a>> {
    let s = get_subject(o)?;
    if pat.is_bytes && !s.is_bytes() {
        raise!(TypeError, "cannot use a bytes pattern on a string-like object");
    }
    if !pat.is_bytes && s.is_bytes() {
        raise!(TypeError, "cannot use a string pattern on a bytes-like object");
    }
    Ok(s)
}

/// `string[a:b]` with the result type rules of `_sre` getslice.
fn getslice(string: &PyObject, a: usize, b: usize) -> PyResult {
    if pystr::is_str(string) {
        return Ok(tu::substr(string, a, b));
    }
    if bytes::is_bytes(string) && string.is_type(bytes::bytes_type()) {
        let d = bytes::bytes_data(string);
        let b2 = b.min(d.len());
        let a2 = a.min(b2);
        if a2 == 0 && b2 == d.len() {
            return Ok(string.to_ref());
        }
        return Ok(bytes::new_bytes(&d[a2..b2]));
    }
    match tu::byte_data(string) {
        Some(d) => {
            let d = d.as_slice();
            let b2 = b.min(d.len());
            let a2 = a.min(b2);
            Ok(bytes::new_bytes(&d[a2..b2]))
        }
        None => raise!(TypeError, "expected string or bytes-like object, got '{}'", tname(string)),
    }
}

fn clamp_range(len: usize, pos: isize, endpos: isize) -> (usize, usize) {
    let l = len as isize;
    let p = pos.clamp(0, l) as usize;
    let e = endpos.clamp(0, l) as usize;
    (p, e)
}

// =====================================================================================
// Pattern object
// =====================================================================================

#[repr(C)]
pub struct PatternObj {
    ob_base: PyObject,
    weakreflist: *mut PyObject,
    pattern: *mut PyObject,
    groupindex: *mut PyObject,
    indexgroup: *mut PyObject,
    flags: i64,
    is_bytes: bool,
    re: *mut rpy_sre::Pattern,
    tcache: *mut Mutex<Vec<(PyRef, std::sync::Arc<Vec<TemplateItem>>)>>,
}

#[repr(C)]
pub struct MatchObj {
    ob_base: PyObject,
    string: *mut PyObject,
    pattern: *mut PyObject,
    regs: *mut PyObject,
    pos: isize,
    endpos: isize,
    lastindex: isize,
    nspans: usize,
    spans: *mut (isize, isize),
}

#[repr(C)]
pub struct ScannerObj {
    ob_base: PyObject,
    pattern: *mut PyObject,
    string: *mut PyObject,
    start_pos: usize,
    pos: usize,
    end: usize,
    must_advance: bool,
    done: bool,
    executing: bool,
    scratch: *mut Scratch,
    held: *mut tu::HeldBuffer,
}

#[repr(C)]
pub struct TemplateObj {
    ob_base: PyObject,
    literals: *mut PyObject,
    indices: *mut PyObject,
}

pub static mut PATTERN_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut MATCH_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut SCANNER_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut TEMPLATE_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn pattern_type() -> *mut PyTypeObject {
    &raw mut PATTERN_TYPE
}
fn match_type() -> *mut PyTypeObject {
    &raw mut MATCH_TYPE
}
fn scanner_type() -> *mut PyTypeObject {
    &raw mut SCANNER_TYPE
}

fn pat(o: &PyObject) -> &PatternObj {
    unsafe { &*(o.as_ptr() as *const PatternObj) }
}
fn mat(o: &PyObject) -> &MatchObj {
    unsafe { &*(o.as_ptr() as *const MatchObj) }
}
fn compiled(p: &PatternObj) -> &rpy_sre::Pattern {
    unsafe { &*p.re }
}

fn check_pattern(o: &PyObject) -> PyResult<&PatternObj> {
    if !o.is_type(pattern_type()) {
        raise!(TypeError, "descriptor requires a 're.Pattern' object but received '{}'", tname(o));
    }
    Ok(pat(o))
}

unsafe extern "C" fn pattern_dealloc(o: *mut PyObject) {
    unsafe {
        let p = &mut *(o as *mut PatternObj);
        if !p.weakreflist.is_null() {
            crate::types::weakref::clear_weakrefs(o);
        }
        xdecref(p.pattern);
        xdecref(p.groupindex);
        xdecref(p.indexgroup);
        if !p.re.is_null() {
            drop(Box::from_raw(p.re));
        }
        if !p.tcache.is_null() {
            drop(Box::from_raw(p.tcache));
        }
        crate::alloc::free_object(o);
    }
}

fn pattern_error_class() -> PyResult {
    tu::import_attr("re", "PatternError")
}

fn raise_compile_error(e: rpy_sre::CompileError, pattern: &PyObject) -> PyErr {
    match e {
        rpy_sre::CompileError::Pattern { msg, pos } => {
            let cls = match pattern_error_class() {
                Ok(c) => c,
                Err(e) => return e,
            };
            let m = pystr::new_str(&msg);
            let p = match pos {
                Some(p) => int::new_int(p as i64),
                None => none::none_ref(),
            };
            match ab::call(&cls, &[&m, pattern, &p]) {
                Ok(exc) => {
                    set_exc(exc);
                    PyErr
                }
                Err(e) => e,
            }
        }
        rpy_sre::CompileError::Overflow(m) => err!(OverflowError, "{}", m),
        rpy_sre::CompileError::Value(m) => err!(ValueError, "{}", m),
    }
}

/// re.DEBUG (tail2): write the parse tree and code listing to sys.stdout.
fn print_debug_dump(pattern: &PyObject, fl: u32) -> PyResult<()> {
    use rpy_sre::PatternSource as PS;
    let r = if pystr::is_str(pattern) {
        match pystr::str_data(pattern) {
            StrData::L1(s) => rpy_sre::debug_text(PS::Latin1(s), fl, &PROVIDER),
            StrData::U2(s) => rpy_sre::debug_text(PS::Ucs2(s), fl, &PROVIDER),
            StrData::U4(s) => rpy_sre::debug_text(PS::Ucs4(s), fl, &PROVIDER),
        }
    } else {
        match tu::byte_data(pattern) {
            Some(b) => rpy_sre::debug_text(PS::Bytes(b.as_slice()), fl, &PROVIDER),
            None => return Ok(()),
        }
    };
    let text = r.map_err(|e| raise_compile_error(e, pattern))?;
    let out = match crate::modules::sys::get_sys_attr("stdout") {
        Some(o) if !o.is(none::none()) => o,
        _ => raise!(RuntimeError, "lost sys.stdout"),
    };
    ab::call_method_str(&out, "write", &[&pystr::new_str(&text)])?;
    Ok(())
}

fn compile_pattern(pattern: &PyObject, flags: i64) -> PyResult {
    use rpy_sre::PatternSource as PS;
    let fl = (flags & 0xffff_ffff) as u32;
    let r = if pystr::is_str(pattern) {
        match pystr::str_data(pattern) {
            StrData::L1(s) => rpy_sre::compile_with(PS::Latin1(s), fl, &PROVIDER),
            StrData::U2(s) => rpy_sre::compile_with(PS::Ucs2(s), fl, &PROVIDER),
            StrData::U4(s) => rpy_sre::compile_with(PS::Ucs4(s), fl, &PROVIDER),
        }
    } else {
        match tu::byte_data(pattern) {
            Some(b) => rpy_sre::compile_with(PS::Bytes(b.as_slice()), fl, &PROVIDER),
            None => raise!(TypeError, "expected string or bytes-like object, got '{}'", tname(pattern)),
        }
    };
    let re = match r {
        Ok(re) => re,
        Err(e) => return Err(raise_compile_error(e, pattern)),
    };
    for w in re.warnings() {
        let cat = match w.kind {
            rpy_sre::WarningKind::FutureWarning => crate::types::exceptions::FutureWarning(),
            rpy_sre::WarningKind::DeprecationWarning => crate::types::exceptions::DeprecationWarning(),
        };
        // user -> re.compile -> re._compile -> re._compiler.compile -> _sre.compile
        tu::warn(cat, &w.message, 4)?;
    }
    if fl & rpy_sre::flags::DEBUG != 0 {
        print_debug_dump(pattern, fl)?;
    }
    let groupindex = dict::new_dict();
    for (name, idx) in re.group_index() {
        dict::set_item(&groupindex, &pystr::new_str(name), &int::new_int(*idx as i64))?;
    }
    let mut ig: Vec<PyRef> = vec![none::none_ref(); re.groups() + 1];
    for (name, idx) in re.group_index() {
        if *idx < ig.len() {
            ig[*idx] = pystr::new_str(name);
        }
    }
    let total_flags = flags | re.flags() as i64;
    let is_bytes = re.is_bytes();
    unsafe {
        let op = crate::alloc::alloc_object(pattern_type(), std::mem::size_of::<PatternObj>());
        let p = &mut *(op as *mut PatternObj);
        p.pattern = pattern.to_ref().into_raw();
        p.groupindex = groupindex.into_raw();
        p.indexgroup = tuple::new_tuple(ig).into_raw();
        p.flags = total_flags;
        p.is_bytes = is_bytes;
        p.re = Box::into_raw(Box::new(re));
        p.tcache = Box::into_raw(Box::new(Mutex::new(Vec::new())));
        Ok(PyRef::from_raw(op))
    }
}

// ------------------------------------------------------------------ matching core

fn new_match(pattern: &PyObject, string: &PyObject, caps: Captures, pos: usize, endpos: usize) -> PyRef {
    let spans = caps.spans.into_boxed_slice();
    let n = spans.len();
    unsafe {
        let op = crate::alloc::alloc_object(match_type(), std::mem::size_of::<MatchObj>());
        let m = &mut *(op as *mut MatchObj);
        m.string = string.to_ref().into_raw();
        m.pattern = pattern.to_ref().into_raw();
        m.pos = pos as isize;
        m.endpos = endpos as isize;
        m.lastindex = caps.lastindex.map_or(-1, |i| i as isize);
        m.nspans = n;
        m.spans = Box::into_raw(spans) as *mut (isize, isize);
        PyRef::from_raw(op)
    }
}

unsafe extern "C" fn match_dealloc(o: *mut PyObject) {
    unsafe {
        let m = &mut *(o as *mut MatchObj);
        xdecref(m.string);
        xdecref(m.pattern);
        xdecref(m.regs);
        if !m.spans.is_null() {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(m.spans, m.nspans)));
        }
        crate::alloc::free_object(o);
    }
}

fn spans(m: &MatchObj) -> &[(isize, isize)] {
    unsafe { std::slice::from_raw_parts(m.spans, m.nspans) }
}

const PNAMES: &[&str] = &["string", "pos", "endpos"];

fn pos_arg(o: Option<&PyObject>, default: isize) -> PyResult<isize> {
    match o {
        None => Ok(default),
        Some(v) => ab::index_as_ssize(v, Some(crate::types::exceptions::OverflowError())),
    }
}

fn do_exec(selfo: &PyObject, args: &[&PyObject], kw: Option<&PyObject>, fname: &str, mode: MatchMode) -> PyResult {
    let p = check_pattern(selfo)?;
    let [s, ps, pe] = args::parse::<3>(&Params::new(fname, PNAMES, 0, 1), args, kw)?;
    let string = s.unwrap();
    let pos = pos_arg(ps, 0)?;
    let endpos = pos_arg(pe, isize::MAX)?;
    let subj = subject_for(p, string)?;
    let u = subj.units();
    let (a, b) = clamp_range(u.len(), pos, endpos);
    if a > b {
        return Ok(none::none_ref());
    }
    let re = compiled(p);
    let caps = dispatch!(u, s => re.exec(s, a, b, mode));
    match caps {
        Some(c) => Ok(new_match(selfo, string, c, a, b)),
        None => Ok(none::none_ref()),
    }
}

fn pattern_match(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    do_exec(o, a, kw, "match", MatchMode::Prefix)
}
fn pattern_prefixmatch(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    do_exec(o, a, kw, "prefixmatch", MatchMode::Prefix)
}
fn pattern_fullmatch(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    do_exec(o, a, kw, "fullmatch", MatchMode::Full)
}
fn pattern_search(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    do_exec(o, a, kw, "search", MatchMode::Search)
}

/// Value of group `g` of a capture set as a Python object (`def` if unmatched).
fn cap_group(string: &PyObject, sp: &[(isize, isize)], g: usize, def: &PyObject) -> PyResult {
    let (a, b) = sp[g];
    if a < 0 {
        return Ok(def.to_ref());
    }
    getslice(string, a as usize, b as usize)
}

fn pattern_findall(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let p = check_pattern(o)?;
    let [s, ps, pe] = args::parse::<3>(&Params::new("findall", PNAMES, 0, 1), args, kw)?;
    let string = s.unwrap();
    let pos = pos_arg(ps, 0)?;
    let endpos = pos_arg(pe, isize::MAX)?;
    let subj = subject_for(p, string)?;
    let u = subj.units();
    let (a, b) = clamp_range(u.len(), pos, endpos);
    let re = compiled(p);
    let all: Vec<Captures> = dispatch!(u, s => re.scanner(s, a, b).collect());
    let out = list::new_list_empty();
    let empty = getslice(string, 0, 0)?;
    let ng = re.groups();
    for c in all {
        let item = match ng {
            0 => cap_group(string, &c.spans, 0, &empty)?,
            1 => cap_group(string, &c.spans, 1, &empty)?,
            _ => {
                let mut v = Vec::with_capacity(ng);
                for g in 1..=ng {
                    v.push(cap_group(string, &c.spans, g, &empty)?);
                }
                tuple::new_tuple(v)
            }
        };
        list::append(&out, item);
    }
    Ok(out)
}

fn new_scanner(pattern: &PyObject, string: &PyObject, pos: isize, endpos: isize) -> PyResult {
    let p = check_pattern(pattern)?;
    let subj = subject_for(p, string)?;
    let len = subj.units().len();
    let (a, b) = clamp_range(len, pos, endpos);
    drop(subj);
    // Hold buffer exports (bytearray, memoryview, ...) for the scanner's lifetime.
    let held = if !pystr::is_str(string) && !bytes::is_bytes(string) {
        match tu::HeldBuffer::acquire(string)? {
            Some(h) => Box::into_raw(Box::new(h)),
            None => std::ptr::null_mut(),
        }
    } else {
        std::ptr::null_mut()
    };
    unsafe {
        let op = crate::alloc::alloc_object(scanner_type(), std::mem::size_of::<ScannerObj>());
        let s = &mut *(op as *mut ScannerObj);
        s.pattern = pattern.to_ref().into_raw();
        s.string = string.to_ref().into_raw();
        s.start_pos = a;
        s.pos = a;
        s.end = b;
        s.done = a > b;
        s.scratch = Box::into_raw(Box::new(Scratch::new()));
        s.held = held;
        Ok(PyRef::from_raw(op))
    }
}

fn pattern_scanner(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    check_pattern(o)?;
    let [s, ps, pe] = args::parse::<3>(&Params::new("scanner", PNAMES, 0, 1), args, kw)?;
    new_scanner(o, s.unwrap(), pos_arg(ps, 0)?, pos_arg(pe, isize::MAX)?)
}

fn pattern_finditer(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    check_pattern(o)?;
    let [s, ps, pe] = args::parse::<3>(&Params::new("finditer", PNAMES, 0, 1), args, kw)?;
    let sc = new_scanner(o, s.unwrap(), pos_arg(ps, 0)?, pos_arg(pe, isize::MAX)?)?;
    let search = ab::getattr_str(&sc, "search")?;
    crate::types::iterobject::new_callable_iter(&search, none::none())
}

fn pattern_split(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let p = check_pattern(o)?;
    let [s, ms] = args::parse::<2>(&Params::new("split", &["string", "maxsplit"], 0, 1), args, kw)?;
    let string = s.unwrap();
    let maxsplit = pos_arg(ms, 0)?;
    let subj = subject_for(p, string)?;
    let u = subj.units();
    let len = u.len();
    let re = compiled(p);
    let mut caps: Vec<Captures> = Vec::new();
    dispatch!(u, s => {
        let mut sc = re.scanner(s, 0, len);
        while maxsplit == 0 || (caps.len() as isize) < maxsplit {
            match sc.search() {
                Some(c) => caps.push(c),
                None => break,
            }
        }
    });
    let out = list::new_list_empty();
    let mut last = 0usize;
    let ng = re.groups();
    for c in &caps {
        list::append(&out, getslice(string, last, c.start())?);
        for g in 1..=ng {
            list::append(&out, cap_group(string, &c.spans, g, none::none())?);
        }
        last = c.end();
    }
    list::append(&out, getslice(string, last, len)?);
    Ok(out)
}

// ------------------------------------------------------------------ templates

fn template_items(po: &PyObject, repl: &PyObject) -> PyResult<std::sync::Arc<Vec<TemplateItem>>> {
    let p = pat(po);
    let cache = unsafe { &*p.tcache };
    {
        let c = cache.lock().unwrap();
        for (k, v) in c.iter() {
            if k.is(repl) {
                return Ok(v.clone());
            }
        }
    }
    let cps: Vec<u32> = if pystr::is_str(repl) {
        pystr::str_data(repl).to_vec()
    } else {
        match tu::byte_data(repl) {
            Some(b) => b.as_slice().iter().map(|&x| x as u32).collect(),
            None => raise!(TypeError, "expected str or bytes-like object, got '{}'", tname(repl)),
        }
    };
    let items = match compiled(p).parse_template(&cps) {
        Ok(it) => it,
        Err(rpy_sre::TemplateError::Index(m)) => raise!(IndexError, "{}", m),
        Err(rpy_sre::TemplateError::Pattern { msg, pos }) => {
            let cls = pattern_error_class()?;
            let exc = ab::call(&cls, &[&pystr::new_str(&msg), repl, &int::new_int(pos as i64)])?;
            set_exc(exc);
            return Err(PyErr);
        }
    };
    let arc = std::sync::Arc::new(items);
    let mut c = cache.lock().unwrap();
    if c.len() >= 8 {
        c.remove(0);
    }
    c.push((repl.to_ref(), arc.clone()));
    Ok(arc)
}

/// Output accumulator for sub/expand (str or bytes).
enum Out {
    S(StrBuf),
    B(Vec<u8>),
}

impl Out {
    fn push_units(&mut self, u: Units, a: usize, b: usize) {
        let b = b.min(u.len());
        let a = a.min(b);
        match self {
            Out::S(sb) => match u {
                Units::B(s) => s[a..b].iter().for_each(|&c| sb.push(c as u32)),
                Units::W(s) => s[a..b].iter().for_each(|&c| sb.push(c as u32)),
                Units::D(s) => s[a..b].iter().for_each(|&c| sb.push(c)),
            },
            Out::B(v) => {
                if let Units::B(s) = u {
                    v.extend_from_slice(&s[a..b]);
                }
            }
        }
    }
    fn push_lit(&mut self, l: &[u32]) {
        match self {
            Out::S(sb) => sb.extend_u32(l),
            Out::B(v) => v.extend(l.iter().map(|&c| c as u8)),
        }
    }
    fn push_obj(&mut self, o: &PyObject, idx: usize) -> PyResult<()> {
        match self {
            Out::S(sb) => {
                if !pystr::is_str(o) {
                    raise!(TypeError, "sequence item {}: expected str instance, {} found", idx, tname(o));
                }
                sb.extend_obj(o);
            }
            Out::B(v) => {
                if pystr::is_str(o) {
                    raise!(TypeError, "sequence item {}: expected a bytes-like object, {} found", idx, tname(o));
                }
                match tu::byte_data(o) {
                    Some(d) => v.extend_from_slice(d.as_slice()),
                    None => raise!(TypeError, "sequence item {}: expected a bytes-like object, {} found", idx, tname(o)),
                }
            }
        }
        Ok(())
    }
    fn finish(self) -> PyRef {
        match self {
            Out::S(sb) => sb.finish(),
            Out::B(v) => bytes::new_bytes(&v),
        }
    }
}

fn expand_items(out: &mut Out, items: &[TemplateItem], u: Units, sp: &[(isize, isize)]) {
    for it in items {
        match it {
            TemplateItem::Literal(l) => out.push_lit(l),
            TemplateItem::Group(g) => {
                if let Some(&(a, b)) = sp.get(*g) {
                    if a >= 0 {
                        out.push_units(u, a as usize, b as usize);
                    }
                }
            }
        }
    }
}

enum Repl {
    Literal(PyRef),
    Template(std::sync::Arc<Vec<TemplateItem>>),
    Callable(PyRef),
}

fn has_backslash(o: &PyObject) -> Option<bool> {
    if pystr::is_str(o) {
        return Some(pystr::str_data(o).iter().any(|c| c == b'\\' as u32));
    }
    tu::byte_data(o).map(|d| d.as_slice().contains(&b'\\'))
}

fn subx(po: &PyObject, repl: &PyObject, string: &PyObject, count: isize, subn: bool) -> PyResult {
    let p = check_pattern(po)?;
    let r = if ab::is_callable(repl) {
        Repl::Callable(repl.to_ref())
    } else {
        match has_backslash(repl) {
            Some(false) => Repl::Literal(repl.to_ref()),
            Some(true) => {
                let items = template_items(po, repl)?;
                if items.iter().all(|i| matches!(i, TemplateItem::Literal(_))) {
                    let mut o = if p.is_bytes { Out::B(Vec::new()) } else { Out::S(StrBuf::new()) };
                    for i in items.iter() {
                        if let TemplateItem::Literal(l) = i {
                            o.push_lit(l);
                        }
                    }
                    Repl::Literal(o.finish())
                } else {
                    Repl::Template(items)
                }
            }
            None => raise!(TypeError, "expected str or bytes-like object, got '{}'", tname(repl)),
        }
    };
    let subj = subject_for(p, string)?;
    let u = subj.units();
    let len = u.len();
    let re = compiled(p);
    let mut out = if p.is_bytes { Out::B(Vec::new()) } else { Out::S(StrBuf::with_capacity(len)) };
    let mut n: isize = 0;
    let mut last = 0usize;
    let mut item_idx = 0usize;
    let mut sc = Scratch::new();
    let mut pos = 0usize;
    let mut must_advance = false;
    while count == 0 || n < count {
        let c = dispatch!(u, s => re.exec_with(&mut sc, s, pos, len, MatchMode::Search, must_advance));
        let c = match c {
            Some(c) => c,
            None => break,
        };
        let (b, e) = (c.start(), c.end());
        if last < b {
            out.push_units(u, last, b);
            item_idx += 1;
        }
        match &r {
            Repl::Literal(l) => {
                out.push_obj(l, item_idx)?;
                item_idx += 1;
            }
            Repl::Template(items) => {
                expand_items(&mut out, items, u, &c.spans);
                item_idx += 1;
            }
            Repl::Callable(f) => {
                let m = new_match(po, string, c, 0, len);
                let item = ab::call(f, &[&m])?;
                if !none::is_none(&item) {
                    out.push_obj(&item, item_idx)?;
                    item_idx += 1;
                }
            }
        }
        last = e;
        must_advance = b == e;
        pos = e;
        n += 1;
    }
    let result = if n == 0 && (string.is_type(pystr::str_type()) || string.is_type(bytes::bytes_type())) {
        string.to_ref()
    } else {
        if last < len {
            out.push_units(u, last, len);
        }
        out.finish()
    };
    if subn {
        Ok(tuple::new_tuple(vec![result, int::new_int(n as i64)]))
    } else {
        Ok(result)
    }
}

fn pattern_sub(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [r, s, c] = args::parse::<3>(&Params::new("sub", &["repl", "string", "count"], 0, 2), args, kw)?;
    subx(o, r.unwrap(), s.unwrap(), pos_arg(c, 0)?, false)
}
fn pattern_subn(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [r, s, c] = args::parse::<3>(&Params::new("subn", &["repl", "string", "count"], 0, 2), args, kw)?;
    subx(o, r.unwrap(), s.unwrap(), pos_arg(c, 0)?, true)
}

// ------------------------------------------------------------------ pattern attributes

fn pattern_get_pattern(o: &PyObject) -> PyResult {
    let p = pat(o);
    Ok(if p.pattern.is_null() { none::none_ref() } else { unsafe { (*p.pattern).to_ref() } })
}
fn pattern_get_flags(o: &PyObject) -> PyResult {
    Ok(int::new_int(pat(o).flags))
}
fn pattern_get_groups(o: &PyObject) -> PyResult {
    Ok(int::new_int(compiled(pat(o)).groups() as i64))
}
fn pattern_get_groupindex(o: &PyObject) -> PyResult {
    let gi = unsafe { &*pat(o).groupindex };
    match tu::import_attr("types", "MappingProxyType") {
        Ok(mp) => ab::call(&mp, &[gi]),
        Err(_) => {
            clear_exc();
            dict::copy(gi)
        }
    }
}

fn pattern_copy(o: &PyObject) -> PyResult {
    Ok(o.to_ref())
}
fn pattern_deepcopy(o: &PyObject, _memo: &PyObject) -> PyResult {
    Ok(o.to_ref())
}
fn class_getitem(cls: &PyObject, item: &PyObject) -> PyResult {
    tu::generic_alias(cls, item)
}

const FLAG_NAMES: &[(&str, i64)] = &[
    ("re.IGNORECASE", 2),
    ("re.LOCALE", 4),
    ("re.MULTILINE", 8),
    ("re.DOTALL", 16),
    ("re.UNICODE", 32),
    ("re.VERBOSE", 64),
    ("re.DEBUG", 128),
    ("re.ASCII", 256),
];

unsafe extern "C" fn pattern_repr(o: *mut PyObject) -> *mut PyObject {
    let o = unsafe { &*o };
    into_c((|| {
        let p = pat(o);
        let po = unsafe { &*p.pattern };
        let r = tu::trunc_chars(&ab::repr_string(po)?, 200);
        let mut flags = p.flags;
        if pystr::is_str(po) && (flags & (4 | 32 | 256)) == 32 {
            flags &= !32;
        }
        let mut parts: Vec<String> = Vec::new();
        for (n, v) in FLAG_NAMES {
            if flags & v != 0 {
                parts.push(n.to_string());
                flags &= !v;
            }
        }
        if flags != 0 {
            parts.push(format!("0x{:x}", flags));
        }
        if parts.is_empty() {
            Ok(pystr::new_str(&format!("re.compile({})", r)))
        } else {
            Ok(pystr::new_str(&format!("re.compile({}, {})", r, parts.join("|"))))
        }
    })())
}

unsafe extern "C" fn pattern_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if (op != Py_EQ && op != Py_NE) || !b.is_type(pattern_type()) {
            return none::not_implemented().to_ref().into_raw();
        }
        let eq = if a.is(b) {
            true
        } else {
            let (pa, pb) = (pat(a), pat(b));
            if pa.flags != pb.flags || pa.is_bytes != pb.is_bytes {
                false
            } else {
                match ab::rich_eq(&*pa.pattern, &*pb.pattern) {
                    Ok(v) => v,
                    Err(_) => return std::ptr::null_mut(),
                }
            }
        };
        bool_::new_bool(if op == Py_EQ { eq } else { !eq }).into_raw()
    }
}

unsafe extern "C" fn pattern_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let p = pat(&*o);
        let h = match ab::hash(&*p.pattern) {
            Ok(h) => h,
            Err(_) => return -1,
        };
        let mut h = h ^ (p.flags as isize) ^ (p.is_bytes as isize);
        if h == -1 {
            h = -2;
        }
        h
    }
}

// =====================================================================================
// Match object
// =====================================================================================

fn check_match(o: &PyObject) -> PyResult<&MatchObj> {
    if !o.is_type(match_type()) {
        raise!(TypeError, "descriptor requires a 're.Match' object but received '{}'", tname(o));
    }
    Ok(mat(o))
}

fn match_pattern(m: &MatchObj) -> &PatternObj {
    unsafe { pat(&*m.pattern) }
}

/// Resolve a group reference (int or name) to an index.
fn match_getindex(m: &MatchObj, idx: &PyObject) -> PyResult<usize> {
    let n = m.nspans;
    if int::is_int(idx) || (ab::has_index(idx) && !pystr::is_str(idx)) {
        let i = ab::index_as_ssize(idx, None)?;
        if i < 0 || i as usize >= n {
            raise!(IndexError, "no such group");
        }
        return Ok(i as usize);
    }
    let p = match_pattern(m);
    let gi = unsafe { &*p.groupindex };
    if pystr::is_str(idx) || bytes::is_bytes(idx) || !ab::hash(idx).is_err() {
        if let Some(v) = dict::get_item(gi, idx)? {
            if int::is_int(&v) {
                let i = ab::index_as_ssize(&v, None)?;
                if i >= 0 && (i as usize) < n {
                    return Ok(i as usize);
                }
            }
        }
        raise!(IndexError, "no such group");
    }
    Err(PyErr)
}

fn match_group_value(mo: &PyObject, i: usize, def: &PyObject) -> PyResult {
    let m = mat(mo);
    cap_group(unsafe { &*m.string }, spans(m), i, def)
}

fn match_group(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let m = check_match(o)?;
    match args.len() {
        0 => match_group_value(o, 0, none::none()),
        1 => {
            let i = match_getindex(m, args[0])?;
            match_group_value(o, i, none::none())
        }
        _ => {
            let mut v = Vec::with_capacity(args.len());
            for a in args {
                let i = match_getindex(m, a)?;
                v.push(match_group_value(o, i, none::none())?);
            }
            Ok(tuple::new_tuple(v))
        }
    }
}

unsafe extern "C" fn match_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (o, k) = (&*o, &*k);
        into_c((|| {
            let i = match_getindex(mat(o), k)?;
            match_group_value(o, i, none::none())
        })())
    }
}

fn match_groups(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let m = check_match(o)?;
    let [d] = args::parse::<1>(&Params::new("groups", &["default"], 0, 0), args, kw)?;
    let def = d.unwrap_or(none::none());
    let mut v = Vec::with_capacity(m.nspans.saturating_sub(1));
    for i in 1..m.nspans {
        v.push(match_group_value(o, i, def)?);
    }
    Ok(tuple::new_tuple(v))
}

fn match_groupdict(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let m = check_match(o)?;
    let [d] = args::parse::<1>(&Params::new("groupdict", &["default"], 0, 0), args, kw)?;
    let def = d.unwrap_or(none::none());
    let out = dict::new_dict();
    let p = match_pattern(m);
    for (name, idx) in compiled(p).group_index() {
        let v = match_group_value(o, *idx, def)?;
        dict::set_item(&out, &pystr::new_str(name), &v)?;
    }
    Ok(out)
}

fn group_arg(m: &MatchObj, args: &[&PyObject], fname: &str) -> PyResult<usize> {
    args::check_pos(fname, args, 0, 1)?;
    match args.first() {
        None => Ok(0),
        Some(a) => match_getindex(m, a),
    }
}

fn match_start(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let m = check_match(o)?;
    let i = group_arg(m, args, "start")?;
    Ok(int::new_int(spans(m)[i].0 as i64))
}
fn match_end(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let m = check_match(o)?;
    let i = group_arg(m, args, "end")?;
    Ok(int::new_int(spans(m)[i].1 as i64))
}
fn span_tuple(sp: (isize, isize)) -> PyRef {
    tuple::new_tuple(vec![int::new_int(sp.0 as i64), int::new_int(sp.1 as i64)])
}
fn match_span(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let m = check_match(o)?;
    let i = group_arg(m, args, "span")?;
    Ok(span_tuple(spans(m)[i]))
}

fn match_expand(o: &PyObject, template: &PyObject) -> PyResult {
    let m = check_match(o)?;
    let p = match_pattern(m);
    if p.is_bytes {
        if pystr::is_str(template) {
            raise!(TypeError, "expected a bytes-like object, str found");
        }
    } else if !pystr::is_str(template) {
        raise!(TypeError, "expected str instance, {} found", tname(template));
    }
    let items = template_items(unsafe { &*m.pattern }, template)?;
    let string = unsafe { &*m.string };
    let subj = get_subject(string)?;
    let u = subj.units();
    let mut out = if p.is_bytes { Out::B(Vec::new()) } else { Out::S(StrBuf::new()) };
    expand_items(&mut out, &items, u, spans(m));
    Ok(out.finish())
}

fn match_copy(o: &PyObject) -> PyResult {
    Ok(o.to_ref())
}
fn match_deepcopy(o: &PyObject, _memo: &PyObject) -> PyResult {
    Ok(o.to_ref())
}

fn match_get_string(o: &PyObject) -> PyResult {
    Ok(unsafe { (*mat(o).string).to_ref() })
}
fn match_get_re(o: &PyObject) -> PyResult {
    Ok(unsafe { (*mat(o).pattern).to_ref() })
}
fn match_get_pos(o: &PyObject) -> PyResult {
    Ok(int::new_int(mat(o).pos as i64))
}
fn match_get_endpos(o: &PyObject) -> PyResult {
    Ok(int::new_int(mat(o).endpos as i64))
}
fn match_get_lastindex(o: &PyObject) -> PyResult {
    let m = mat(o);
    Ok(if m.lastindex >= 0 { int::new_int(m.lastindex as i64) } else { none::none_ref() })
}
fn match_get_lastgroup(o: &PyObject) -> PyResult {
    let m = mat(o);
    if m.lastindex < 0 {
        return Ok(none::none_ref());
    }
    let p = match_pattern(m);
    let ig = unsafe { &*p.indexgroup };
    let i = m.lastindex as usize;
    if i < tuple::tuple_len(ig) {
        return Ok(tuple::tuple_get(ig, i).to_ref());
    }
    Ok(none::none_ref())
}
fn match_get_regs(o: &PyObject) -> PyResult {
    let m = mat(o);
    if !m.regs.is_null() {
        return Ok(unsafe { (*m.regs).to_ref() });
    }
    let v: Vec<PyRef> = spans(m).iter().map(|&s| span_tuple(s)).collect();
    let t = tuple::new_tuple(v);
    unsafe {
        let mm = &mut *(o.as_ptr() as *mut MatchObj);
        mm.regs = t.clone().into_raw();
    }
    Ok(t)
}

unsafe extern "C" fn match_repr(o: *mut PyObject) -> *mut PyObject {
    let o = unsafe { &*o };
    into_c((|| {
        let m = mat(o);
        let g0 = match_group_value(o, 0, none::none())?;
        let r = tu::trunc_chars(&ab::repr_string(&g0)?, 50);
        let sp = spans(m)[0];
        Ok(pystr::new_str(&format!(
            "<{} object; span=({}, {}), match={}>",
            unsafe { std::ffi::CStr::from_ptr(o.ty().tp_name) }.to_string_lossy(),
            sp.0,
            sp.1,
            r
        )))
    })())
}

// =====================================================================================
// Scanner
// =====================================================================================

unsafe extern "C" fn scanner_dealloc(o: *mut PyObject) {
    unsafe {
        let s = &mut *(o as *mut ScannerObj);
        xdecref(s.pattern);
        xdecref(s.string);
        if !s.scratch.is_null() {
            drop(Box::from_raw(s.scratch));
        }
        if !s.held.is_null() {
            drop(Box::from_raw(s.held));
        }
        crate::alloc::free_object(o);
    }
}

fn scanner_step(o: &PyObject, mode: MatchMode) -> PyResult {
    let s = unsafe { &mut *(o.as_ptr() as *mut ScannerObj) };
    if s.done {
        return Ok(none::none_ref());
    }
    if s.executing {
        raise!(ValueError, "regular expression scanner already executing");
    }
    let po = unsafe { &*s.pattern };
    let string = unsafe { &*s.string };
    let p = pat(po);
    let held_subj;
    let u = if !s.held.is_null() {
        Units::B(unsafe { (*s.held).as_slice() })
    } else {
        held_subj = subject_for(p, string)?;
        held_subj.units()
    };
    let end = s.end.min(u.len());
    let re = compiled(p);
    s.executing = true;
    let sc = unsafe { &mut *s.scratch };
    let c = dispatch!(u, st => re.exec_with(sc, st, s.pos, end, mode, s.must_advance));
    s.executing = false;
    match c {
        Some(c) => {
            let (b, e) = (c.start(), c.end());
            s.must_advance = b == e;
            s.pos = e;
            Ok(new_match(po, string, c, s.start_pos, end))
        }
        None => {
            s.done = true;
            Ok(none::none_ref())
        }
    }
}

fn scanner_match(o: &PyObject) -> PyResult {
    scanner_step(o, MatchMode::Prefix)
}
fn scanner_search(o: &PyObject) -> PyResult {
    scanner_step(o, MatchMode::Search)
}
fn scanner_get_pattern(o: &PyObject) -> PyResult {
    Ok(unsafe { (*(*(o.as_ptr() as *mut ScannerObj)).pattern).to_ref() })
}

// =====================================================================================
// Template
// =====================================================================================

unsafe extern "C" fn template_dealloc(o: *mut PyObject) {
    unsafe {
        let t = &mut *(o as *mut TemplateObj);
        xdecref(t.literals);
        xdecref(t.indices);
        crate::alloc::free_object(o);
    }
}

fn sre_template(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("template", args, 2, 2)?;
    let pattern = args[0];
    let tl = args[1];
    if !list::is_list(tl) {
        raise!(TypeError, "template() argument 2 must be list, not {}", tname(tl));
    }
    let items = list::list_to_vec(tl);
    if items.len() % 2 == 0 {
        raise!(TypeError, "invalid template");
    }
    let mut lits = Vec::new();
    let mut idx = Vec::new();
    for (i, it) in items.iter().enumerate() {
        if i % 2 == 0 {
            lits.push(it.clone());
        } else {
            if !int::is_int(it) && !ab::has_index(it) {
                raise!(TypeError, "an integer is required");
            }
            let v = ab::index_as_ssize(it, Some(crate::types::exceptions::OverflowError()))?;
            if v < 0 {
                raise!(TypeError, "invalid template");
            }
            idx.push(int::new_int(v as i64));
        }
    }
    let _ = pattern;
    unsafe {
        let t = &raw mut TEMPLATE_TYPE;
        let op = crate::alloc::alloc_object(t, std::mem::size_of::<TemplateObj>());
        let to = &mut *(op as *mut TemplateObj);
        to.literals = tuple::new_tuple(lits).into_raw();
        to.indices = tuple::new_tuple(idx).into_raw();
        Ok(PyRef::from_raw(op))
    }
}

// =====================================================================================
// Module functions
// =====================================================================================

fn sre_compile(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [pattern, flags, code, _groups, _groupindex, _indexgroup] = args::parse::<6>(
        &Params::new("compile", &["pattern", "flags", "code", "groups", "groupindex", "indexgroup"], 0, 6),
        args,
        kw,
    )?;
    let (pattern, flags, code) = (pattern.unwrap(), flags.unwrap(), code.unwrap());
    let flags = ab::as_i64(flags)?;
    if !list::is_list(code) {
        raise!(TypeError, "compile() argument 'code' must be list, not {}", tname(code));
    }
    for c in list::list_to_vec(code) {
        let i = ab::index(&c)?;
        match int::as_i128(&i) {
            Some(v) if v < 0 => raise!(OverflowError, "can't convert negative value to unsigned int"),
            Some(v) if v > u32::MAX as i128 => raise!(OverflowError, "regular expression code size limit exceeded"),
            Some(_) => {}
            None => raise!(OverflowError, "Python int too large to convert to C unsigned long"),
        }
    }
    if !pystr::is_str(pattern) && !tu::is_buffer(pattern) {
        raise!(TypeError, "expected string or bytes-like object, got '{}'", tname(pattern));
    }
    compile_pattern(pattern, flags)
}

fn sre_getcodesize(_m: &PyObject) -> PyResult {
    Ok(int::new_int(rpy_sre::CODESIZE as i64))
}

fn char_arg(o: &PyObject) -> PyResult<u32> {
    let v = ab::as_i64(o)?;
    Ok(v as u32)
}
fn sre_ascii_iscased(_m: &PyObject, c: &PyObject) -> PyResult {
    let c = char_arg(c)?;
    Ok(bool_::new_bool(c < 128 && (c as u8).is_ascii_alphabetic()))
}
fn sre_unicode_iscased(_m: &PyObject, c: &PyObject) -> PyResult {
    let c = char_arg(c)?;
    Ok(bool_::new_bool(c != rpy_unicode::py_to_lowercase(c) || c != rpy_unicode::py_to_uppercase(c)))
}
fn sre_ascii_tolower(_m: &PyObject, c: &PyObject) -> PyResult {
    let c = char_arg(c)?;
    Ok(int::new_int(if c < 128 { (c as u8).to_ascii_lowercase() as i64 } else { c as i64 }))
}
fn sre_unicode_tolower(_m: &PyObject, c: &PyObject) -> PyResult {
    let c = char_arg(c)?;
    Ok(int::new_int(rpy_unicode::py_to_lowercase(c) as i64))
}

// =====================================================================================
// Types and module init
// =====================================================================================

static MAPPING: std::sync::atomic::AtomicPtr<PyMappingMethods> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

fn init_types() -> PyResult<()> {
    static DONE: OnceLock<()> = OnceLock::new();
    if DONE.get().is_some() {
        return Ok(());
    }
    unsafe {
        let flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_DISALLOW_INSTANTIATION;
        let t = &mut *pattern_type();
        t.tp_name = c"re.Pattern".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PatternObj>() as isize;
        t.tp_flags = flags;
        t.tp_dealloc = Some(pattern_dealloc);
        t.tp_repr = Some(pattern_repr);
        t.tp_hash = Some(pattern_hash);
        t.tp_richcompare = Some(pattern_richcompare);
        t.tp_weaklistoffset = std::mem::offset_of!(PatternObj, weakreflist) as isize;
        t.tp_doc = c"Compiled regular expression object.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("match", FASTKW, pattern_match, "Same as prefixmatch(): match at the start of the string."),
            ("prefixmatch", FASTKW, pattern_prefixmatch, "prefixmatch($self, /, string, pos=0, endpos=sys.maxsize)\n--\n\nTry to match the pattern at the start of string[pos:endpos]; return a Match or None."),
            ("fullmatch", FASTKW, pattern_fullmatch, "fullmatch($self, /, string, pos=0, endpos=sys.maxsize)\n--\n\nMatch only if the pattern covers the whole of string[pos:endpos]."),
            ("search", FASTKW, pattern_search, "search($self, /, string, pos=0, endpos=sys.maxsize)\n--\n\nReturn a Match for the first place the pattern matches, or None."),
            ("sub", FASTKW, pattern_sub, "sub($self, /, repl, string, count=0)\n--\n\nReturn string with up to count (0: all) non-overlapping matches replaced by repl.\n\nrepl is a string (backslash escapes are processed) or a callable that\nreceives the Match and returns the replacement."),
            ("subn", FASTKW, pattern_subn, "subn($self, /, repl, string, count=0)\n--\n\nLike sub(), but return (new_string, number_of_substitutions)."),
            ("findall", FASTKW, pattern_findall, "findall($self, /, string, pos=0, endpos=sys.maxsize)\n--\n\nReturn a list of all non-overlapping matches (or of their groups)."),
            ("split", FASTKW, pattern_split, "split($self, /, string, maxsplit=0)\n--\n\nSplit string at the matches; captured groups are kept in the result."),
            ("finditer", FASTKW, pattern_finditer, "finditer($self, /, string, pos=0, endpos=sys.maxsize)\n--\n\nReturn an iterator yielding a Match for each non-overlapping match."),
            ("scanner", FASTKW, pattern_scanner, "scanner($self, /, string, pos=0, endpos=sys.maxsize)\n--\n\n"),
            ("__copy__", NOARGS, pattern_copy),
            ("__deepcopy__", O, pattern_deepcopy, "__deepcopy__($self, memo, /)\n--\n\n"),
            ("__class_getitem__", O, class_getitem, "See PEP 585"; METH_CLASS),
        ];
        t.tp_getset = crate::getset_table![
            ("pattern", pattern_get_pattern; "The pattern string from which the RE object was compiled."),
            ("flags", pattern_get_flags; "The regex matching flags."),
            ("groups", pattern_get_groups; "The number of capturing groups in the pattern."),
            ("groupindex", pattern_get_groupindex; "A dictionary mapping group names to group numbers."),
        ];

        let mp = Box::leak(Box::new(PyMappingMethods::ZERO));
        mp.mp_subscript = Some(match_subscript);
        MAPPING.store(mp, std::sync::atomic::Ordering::Release);
        let t = &mut *match_type();
        t.tp_name = c"re.Match".as_ptr();
        t.tp_basicsize = std::mem::size_of::<MatchObj>() as isize;
        t.tp_flags = flags;
        t.tp_dealloc = Some(match_dealloc);
        t.tp_repr = Some(match_repr);
        t.tp_as_mapping = mp;
        t.tp_doc = c"The result of re.match() and re.search().\nMatch objects always have a boolean value of True.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("group", FAST, match_group, "group([group1, ...]) -> str or tuple.\n    Return subgroup(s) of the match by indices or names.\n    For 0 returns the entire match."),
            ("start", FAST, match_start, "start($self, group=0, /)\n--\n\nReturn the start index of the text matched by group (-1 if it did not take part)."),
            ("end", FAST, match_end, "end($self, group=0, /)\n--\n\nReturn the end index of the text matched by group (-1 if it did not take part)."),
            ("span", FAST, match_span, "span($self, group=0, /)\n--\n\nReturn (start(group), end(group))."),
            ("groups", FASTKW, match_groups, "groups($self, /, default=None)\n--\n\nReturn a tuple of all groups from 1 on; default stands in for unmatched groups."),
            ("groupdict", FASTKW, match_groupdict, "groupdict($self, /, default=None)\n--\n\nReturn a dict mapping group names to the matched text (default if unmatched)."),
            ("expand", O, match_expand, "expand($self, template, /)\n--\n\nSubstitute the match into template, as sub() does with its repl string."),
            ("__copy__", NOARGS, match_copy),
            ("__deepcopy__", O, match_deepcopy, "__deepcopy__($self, memo, /)\n--\n\n"),
            ("__class_getitem__", O, class_getitem, "See PEP 585"; METH_CLASS),
        ];
        t.tp_getset = crate::getset_table![
            ("string", match_get_string; "The string passed to match() or search()."),
            ("re", match_get_re; "The regular expression object."),
            ("pos", match_get_pos; "The index into the string at which the RE engine started looking for a match."),
            ("endpos", match_get_endpos; "The index into the string beyond which the RE engine will not go."),
            ("lastindex", match_get_lastindex; "The integer index of the last matched capturing group."),
            ("lastgroup", match_get_lastgroup; "The name of the last matched capturing group."),
            ("regs", match_get_regs),
        ];

        let t = &mut *scanner_type();
        t.tp_name = c"_sre.SRE_Scanner".as_ptr();
        t.tp_basicsize = std::mem::size_of::<ScannerObj>() as isize;
        t.tp_flags = flags;
        t.tp_dealloc = Some(scanner_dealloc);
        t.tp_methods = crate::mdefs![("match", NOARGS, scanner_match), ("search", NOARGS, scanner_search),];
        t.tp_getset = crate::getset_table![("pattern", scanner_get_pattern)];

        let t = &mut *(&raw mut TEMPLATE_TYPE);
        t.tp_name = c"_sre.SRE_Template".as_ptr();
        t.tp_basicsize = std::mem::size_of::<TemplateObj>() as isize;
        t.tp_flags = flags;
        t.tp_dealloc = Some(template_dealloc);

        for t in [pattern_type(), match_type(), scanner_type(), &raw mut TEMPLATE_TYPE] {
            tu::ready_type(t)?;
        }
    }
    let _ = type_ready;
    let _ = DONE.set(());
    Ok(())
}

pub fn init_module() -> PyResult {
    init_types()?;
    let m = crate::types::module::new_module("_sre")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("compile", FASTKW, sre_compile, "compile($module, /, pattern, flags, code, groupindex, indexgroup)\n--\n\n"),
            ("template", FAST, sre_template, "template($module, pattern, template, /)\n--\n\nBuild a compiled replacement template.\n\ntemplate is a list alternating literal strings and group indices."),
            ("getcodesize", NOARGS, sre_getcodesize),
            ("ascii_iscased", O, sre_ascii_iscased, "ascii_iscased($module, character, /)\n--\n\n"),
            ("unicode_iscased", O, sre_unicode_iscased, "unicode_iscased($module, character, /)\n--\n\n"),
            ("ascii_tolower", O, sre_ascii_tolower, "ascii_tolower($module, character, /)\n--\n\n"),
            ("unicode_tolower", O, sre_unicode_tolower, "unicode_tolower($module, character, /)\n--\n\n"),
        ],
        Some(&m),
    )?;
    dict::set_item_str(&d, "MAGIC", &int::new_int(rpy_sre::MAGIC as i64))?;
    dict::set_item_str(&d, "CODESIZE", &int::new_int(rpy_sre::CODESIZE as i64))?;
    dict::set_item_str(&d, "MAXREPEAT", &int::new_int(rpy_sre::MAXREPEAT as i64))?;
    dict::set_item_str(&d, "MAXGROUPS", &int::new_int(rpy_sre::MAXGROUPS as i64))?;
    dict::set_item_str(&d, "copyright", &pystr::new_str(" SRE 2.2.2 Copyright (c) 1997-2002 by Secret Labs AB "))?;
    dict::set_item_str(&d, "Pattern", crate::types::typeobject::type_obj(pattern_type()))?;
    dict::set_item_str(&d, "Match", crate::types::typeobject::type_obj(match_type()))?;
    Ok(m)
}
