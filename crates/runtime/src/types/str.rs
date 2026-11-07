//! `str` core: PEP 393 compact representation (layout forced by the ABI's inline
//! accessors), creation, views, hashing, interning. String *methods* live in
//! `str_methods.rs`.

use crate::alloc;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use std::sync::atomic::Ordering::*;

pub const KIND_1: u32 = 1;
pub const KIND_2: u32 = 2;
pub const KIND_4: u32 = 4;

const STATE_COMPACT: u32 = 1 << 11;
const STATE_ASCII: u32 = 1 << 12;
const STATE_STATIC: u32 = 1 << 13;

#[unsafe(no_mangle)]
pub static mut PyUnicode_Type: PyTypeObject = PyTypeObject::ZERO;

#[inline(always)]
pub fn str_type() -> *mut PyTypeObject {
    &raw mut PyUnicode_Type
}

#[inline(always)]
pub fn is_str(o: &PyObject) -> bool {
    o.is_type(str_type()) || o.ty().tp_flags & Py_TPFLAGS_UNICODE_SUBCLASS != 0
}
#[inline(always)]
pub fn is_str_exact(o: &PyObject) -> bool {
    o.is_type(str_type())
}

#[derive(Clone, Copy)]
pub enum StrData<'a> {
    L1(&'a [u8]),
    U2(&'a [u16]),
    U4(&'a [u32]),
}

impl<'a> StrData<'a> {
    #[inline(always)]
    pub fn len(&self) -> usize {
        match self {
            StrData::L1(s) => s.len(),
            StrData::U2(s) => s.len(),
            StrData::U4(s) => s.len(),
        }
    }
    #[inline(always)]
    pub fn get(&self, i: usize) -> u32 {
        match self {
            StrData::L1(s) => s[i] as u32,
            StrData::U2(s) => s[i] as u32,
            StrData::U4(s) => s[i],
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = u32> + 'a {
        let d = *self;
        (0..d.len()).map(move |i| d.get(i))
    }
    pub fn kind(&self) -> u32 {
        match self {
            StrData::L1(_) => 1,
            StrData::U2(_) => 2,
            StrData::U4(_) => 4,
        }
    }
    pub fn as_bytes(&self) -> &'a [u8] {
        unsafe {
            match self {
                StrData::L1(s) => s,
                StrData::U2(s) => std::slice::from_raw_parts(s.as_ptr() as *const u8, s.len() * 2),
                StrData::U4(s) => std::slice::from_raw_parts(s.as_ptr() as *const u8, s.len() * 4),
            }
        }
    }
    pub fn to_vec(&self) -> Vec<u32> {
        self.iter().collect()
    }
}

#[inline(always)]
fn ascii_obj(o: &PyObject) -> &PyASCIIObject {
    unsafe { o.cast::<PyASCIIObject>() }
}

#[inline(always)]
pub fn str_len(o: &PyObject) -> usize {
    ascii_obj(o).length as usize
}

#[inline(always)]
pub fn str_kind(o: &PyObject) -> u32 {
    (ascii_obj(o).state >> 8) & 7
}

#[inline(always)]
pub fn str_is_ascii(o: &PyObject) -> bool {
    ascii_obj(o).state & STATE_ASCII != 0
}

#[inline(always)]
pub fn str_data_ptr(o: &PyObject) -> *mut u8 {
    let st = ascii_obj(o).state;
    let p = o.as_ptr() as *mut u8;
    unsafe {
        if st & STATE_COMPACT != 0 {
            if st & STATE_ASCII != 0 {
                p.add(std::mem::size_of::<PyASCIIObject>())
            } else {
                p.add(std::mem::size_of::<PyCompactUnicodeObject>())
            }
        } else {
            (*(o.as_ptr() as *mut PyUnicodeObject)).data as *mut u8
        }
    }
}

#[inline(always)]
pub fn str_data(o: &PyObject) -> StrData<'_> {
    let len = str_len(o);
    let p = str_data_ptr(o);
    unsafe {
        match str_kind(o) {
            KIND_1 => StrData::L1(std::slice::from_raw_parts(p, len)),
            KIND_2 => StrData::U2(std::slice::from_raw_parts(p as *const u16, len)),
            _ => StrData::U4(std::slice::from_raw_parts(p as *const u32, len)),
        }
    }
}

/// Fast path: the string as `&str` if it is pure ASCII.
#[inline(always)]
pub fn ascii_str(o: &PyObject) -> Option<&str> {
    if str_is_ascii(o) {
        let len = str_len(o);
        unsafe { Some(std::str::from_utf8_unchecked(std::slice::from_raw_parts(str_data_ptr(o), len))) }
    } else {
        None
    }
}

/// Allocate an uninitialised compact string of `len` chars with the given maximum char.
pub fn alloc_str(len: usize, maxchar: u32) -> PyRef {
    try_alloc_str(len, maxchar).expect("out of memory allocating str")
}

/// Like `alloc_str`, but returns None when the allocation fails.
pub fn try_alloc_str(len: usize, maxchar: u32) -> Option<PyRef> {
    let (kind, ascii) = if maxchar < 0x80 {
        (1u32, true)
    } else if maxchar < 0x100 {
        (1, false)
    } else if maxchar < 0x10000 {
        (2, false)
    } else {
        (4, false)
    };
    let header = if ascii { std::mem::size_of::<PyASCIIObject>() } else { std::mem::size_of::<PyCompactUnicodeObject>() };
    if len >= (isize::MAX as usize - header) / kind as usize - 1 {
        return None; // no such string fits in memory (and the size would overflow)
    }
    let size = header + (len + 1) * kind as usize;
    unsafe {
        let op = alloc::alloc_object(str_type(), size);
        if op.is_null() {
            return None;
        }
        let a = &mut *(op as *mut PyASCIIObject);
        a.length = len as isize;
        a.hash = -1;
        a.state = (kind << 8) | STATE_COMPACT | if ascii { STATE_ASCII } else { 0 };
        if !ascii {
            let c = &mut *(op as *mut PyCompactUnicodeObject);
            c.utf8_length = 0;
            c.utf8 = std::ptr::null_mut();
        }
        Some(PyRef::from_raw(op))
    }
}

#[inline]
fn empty_str() -> PyRef {
    static EMPTY: std::sync::atomic::AtomicPtr<PyObject> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
    let p = EMPTY.load(Acquire);
    if !p.is_null() {
        return unsafe { PyRef::from_borrowed(p) };
    }
    let s = alloc_str(0, 0);
    unsafe { make_immortal(s.as_ptr()) };
    match EMPTY.compare_exchange(std::ptr::null_mut(), s.as_ptr(), AcqRel, Acquire) {
        Ok(_) => s,
        Err(other) => unsafe { PyRef::from_borrowed(other) },
    }
}

/// Single-character Latin-1 strings are cached (immortal).
pub fn latin1_char(c: u8) -> PyRef {
    static CACHE: [std::sync::atomic::AtomicPtr<PyObject>; 256] =
        [const { std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()) }; 256];
    let p = CACHE[c as usize].load(Acquire);
    if !p.is_null() {
        return unsafe { PyRef::from_borrowed(p) };
    }
    let s = alloc_str(1, c as u32);
    unsafe {
        *str_data_ptr(&s) = c;
        make_immortal(s.as_ptr());
    }
    match CACHE[c as usize].compare_exchange(std::ptr::null_mut(), s.as_ptr(), AcqRel, Acquire) {
        Ok(_) => s,
        Err(other) => unsafe { PyRef::from_borrowed(other) },
    }
}

/// Create a str from Rust UTF-8.
pub fn new_str(s: &str) -> PyRef {
    let b = s.as_bytes();
    if b.is_empty() {
        return empty_str();
    }
    if b.is_ascii() {
        if b.len() == 1 {
            return latin1_char(b[0]);
        }
        let r = alloc_str(b.len(), 0x7f);
        unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), str_data_ptr(&r), b.len()) };
        return r;
    }
    let mut maxc = 0u32;
    let mut n = 0usize;
    for c in s.chars() {
        maxc = maxc.max(c as u32);
        n += 1;
    }
    // (tail2) one Latin-1 character: the shared singleton, as for ASCII
    if n == 1 && maxc < 256 {
        return latin1_char(maxc as u8);
    }
    let r = alloc_str(n, maxc);
    let p = str_data_ptr(&r);
    unsafe {
        match str_kind(&r) {
            KIND_1 => {
                for (i, c) in s.chars().enumerate() {
                    *p.add(i) = c as u8;
                }
            }
            KIND_2 => {
                let p = p as *mut u16;
                for (i, c) in s.chars().enumerate() {
                    *p.add(i) = c as u16;
                }
            }
            _ => {
                let p = p as *mut u32;
                for (i, c) in s.chars().enumerate() {
                    *p.add(i) = c as u32;
                }
            }
        }
    }
    r
}

/// Create a str from code points (may include lone surrogates).
pub fn from_codepoints(cps: &[u32]) -> PyRef {
    let maxc = cps.iter().copied().max().unwrap_or(0);
    from_codepoints_max(cps, maxc)
}

pub fn from_codepoints_max(cps: &[u32], maxc: u32) -> PyRef {
    if cps.is_empty() {
        return empty_str();
    }
    if cps.len() == 1 && maxc < 256 {
        return latin1_char(cps[0] as u8);
    }
    let r = alloc_str(cps.len(), maxc);
    let p = str_data_ptr(&r);
    unsafe {
        match str_kind(&r) {
            KIND_1 => {
                for (i, &c) in cps.iter().enumerate() {
                    *p.add(i) = c as u8;
                }
            }
            KIND_2 => {
                let p = p as *mut u16;
                for (i, &c) in cps.iter().enumerate() {
                    *p.add(i) = c as u16;
                }
            }
            _ => {
                std::ptr::copy_nonoverlapping(cps.as_ptr(), p as *mut u32, cps.len());
            }
        }
    }
    r
}

pub fn from_latin1(b: &[u8]) -> PyRef {
    if b.is_empty() {
        return empty_str();
    }
    if b.len() == 1 {
        return latin1_char(b[0]);
    }
    let maxc = if b.is_ascii() { 0x7f } else { 0xff };
    let r = alloc_str(b.len(), maxc);
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), str_data_ptr(&r), b.len()) };
    r
}

pub fn from_ucs2(u: &[u16]) -> PyRef {
    let maxc = u.iter().copied().max().unwrap_or(0) as u32;
    if maxc < 256 {
        let v: Vec<u8> = u.iter().map(|&c| c as u8).collect();
        return from_latin1(&v);
    }
    let r = alloc_str(u.len(), maxc);
    unsafe { std::ptr::copy_nonoverlapping(u.as_ptr(), str_data_ptr(&r) as *mut u16, u.len()) };
    r
}

/// Create from a StrData (copies, canonicalising the kind).
pub fn from_data(d: StrData) -> PyRef {
    match d {
        StrData::L1(b) => from_latin1(b),
        StrData::U2(u) => from_ucs2(u),
        StrData::U4(u) => from_codepoints(u),
    }
}

/// Convert to a Rust String; lone surrogates become U+FFFD.
pub fn to_string_lossy(o: &PyObject) -> String {
    if let Some(s) = ascii_str(o) {
        return s.to_owned();
    }
    str_data(o).iter().map(|c| char::from_u32(c).unwrap_or('\u{fffd}')).collect()
}

/// Convert to a Rust String; None if it contains lone surrogates.
pub fn to_string_strict(o: &PyObject) -> Option<String> {
    if let Some(s) = ascii_str(o) {
        return Some(s.to_owned());
    }
    let mut out = String::with_capacity(str_len(o));
    for c in str_data(o).iter() {
        out.push(char::from_u32(c)?);
    }
    Some(out)
}

/// UTF-8 view, cached in the object (PyUnicode_AsUTF8 semantics). Errors on surrogates.
pub fn as_utf8(o: &PyObject) -> PyResult<&str> {
    if let Some(s) = ascii_str(o) {
        return Ok(s);
    }
    unsafe {
        let c = &*(o.as_ptr() as *const PyCompactUnicodeObject);
        let cached = std::ptr::read_volatile(&c.utf8);
        if !cached.is_null() {
            let len = c.utf8_length as usize;
            return Ok(std::str::from_utf8_unchecked(std::slice::from_raw_parts(cached as *const u8, len)));
        }
        let s = match to_string_strict(o) {
            Some(s) => s,
            None => {
                let pos = str_data(o).iter().position(|c| (0xd800..0xe000).contains(&c)).unwrap_or(0);
                return Err(crate::types::exceptions::unicode_encode_error(
                    "utf-8",
                    o,
                    pos,
                    pos + 1,
                    "surrogates not allowed",
                ));
            }
        };
        let len = s.len();
        let buf = alloc::raw_malloc(len + 1);
        std::ptr::copy_nonoverlapping(s.as_ptr(), buf, len);
        *buf.add(len) = 0;
        let cm = o.as_ptr() as *mut PyCompactUnicodeObject;
        let slot = std::sync::atomic::AtomicPtr::<std::ffi::c_char>::from_ptr(&raw mut (*cm).utf8);
        match slot.compare_exchange(std::ptr::null_mut(), buf as *mut _, AcqRel, Acquire) {
            Ok(_) => {
                (*cm).utf8_length = len as isize;
                Ok(std::str::from_utf8_unchecked(std::slice::from_raw_parts(buf, len)))
            }
            Err(_) => {
                alloc::raw_free(buf);
                as_utf8(o)
            }
        }
    }
}

#[inline]
pub fn str_hash(o: &PyObject) -> isize {
    let a = ascii_obj(o);
    let h = unsafe { std::ptr::read_volatile(&a.hash) };
    if h != -1 {
        return h;
    }
    let h = crate::hash::hash_bytes(str_data(o).as_bytes());
    unsafe {
        let hp = &raw mut (*(o.as_ptr() as *mut PyASCIIObject)).hash;
        std::sync::atomic::AtomicIsize::from_ptr(hp).store(h, Relaxed);
    }
    h
}

#[inline]
pub fn str_eq(a: &PyObject, b: &PyObject) -> bool {
    if a.is(b) {
        return true;
    }
    let la = str_len(a);
    if la != str_len(b) || str_kind(a) != str_kind(b) {
        return false;
    }
    let k = str_kind(a) as usize;
    unsafe {
        std::slice::from_raw_parts(str_data_ptr(a), la * k) == std::slice::from_raw_parts(str_data_ptr(b), la * k)
    }
}

pub fn str_eq_ascii(a: &PyObject, s: &str) -> bool {
    match ascii_str(a) {
        Some(x) => x == s,
        None => false,
    }
}

pub fn str_cmp(a: &PyObject, b: &PyObject) -> std::cmp::Ordering {
    let da = str_data(a);
    let db = str_data(b);
    if let (StrData::L1(x), StrData::L1(y)) = (da, db) {
        return x.cmp(y);
    }
    let n = da.len().min(db.len());
    for i in 0..n {
        let (x, y) = (da.get(i), db.get(i));
        if x != y {
            return x.cmp(&y);
        }
    }
    da.len().cmp(&db.len())
}

/// Concatenate two strings.
pub fn concat(a: &PyObject, b: &PyObject) -> PyRef {
    let la = str_len(a);
    let lb = str_len(b);
    if la == 0 {
        return b.to_ref();
    }
    if lb == 0 {
        return a.to_ref();
    }
    let maxc = max_char(a).max(max_char(b));
    let r = alloc_str(la + lb, maxc);
    let k = str_kind(&r);
    write_into(&r, 0, str_data(a), k);
    write_into(&r, la, str_data(b), k);
    r
}

/// Maximum code point bound for the representation (cheap, not exact).
pub fn max_char(o: &PyObject) -> u32 {
    if str_is_ascii(o) {
        0x7f
    } else {
        match str_kind(o) {
            KIND_1 => 0xff,
            KIND_2 => 0xffff,
            _ => 0x10ffff,
        }
    }
}

/// Copy `src` into string `dst` (fresh, unshared) at char offset `at`.
pub fn write_into(dst: &PyObject, at: usize, src: StrData, dkind: u32) {
    let p = str_data_ptr(dst);
    unsafe {
        match (dkind, src) {
            (KIND_1, StrData::L1(s)) => std::ptr::copy_nonoverlapping(s.as_ptr(), p.add(at), s.len()),
            (KIND_2, StrData::U2(s)) => std::ptr::copy_nonoverlapping(s.as_ptr(), (p as *mut u16).add(at), s.len()),
            (KIND_4, StrData::U4(s)) => std::ptr::copy_nonoverlapping(s.as_ptr(), (p as *mut u32).add(at), s.len()),
            (KIND_2, s) => {
                let d = (p as *mut u16).add(at);
                for i in 0..s.len() {
                    *d.add(i) = s.get(i) as u16;
                }
            }
            (KIND_4, s) => {
                let d = (p as *mut u32).add(at);
                for i in 0..s.len() {
                    *d.add(i) = s.get(i);
                }
            }
            (_, s) => {
                let d = p.add(at);
                for i in 0..s.len() {
                    *d.add(i) = s.get(i) as u8;
                }
            }
        }
    }
}

/// Builder for strings of unknown final width. Text is collected one byte per
/// character while every character is below U+0100 (the common case: ASCII
/// output of repr/json/format) and switches to 32-bit units on the first wider one.
pub struct StrBuilder {
    narrow: Vec<u8>,
    wide: Vec<u32>,
    is_wide: bool,
    /// upper bound of the characters pushed (exact below 0x100)
    maxc: u32,
}
impl Default for StrBuilder {
    fn default() -> Self {
        Self::new()
    }
}
impl StrBuilder {
    pub fn new() -> Self {
        StrBuilder { narrow: Vec::new(), wide: Vec::new(), is_wide: false, maxc: 0 }
    }
    pub fn with_capacity(n: usize) -> Self {
        StrBuilder { narrow: Vec::with_capacity(n), wide: Vec::new(), is_wide: false, maxc: 0 }
    }
    /// (capi3) Would appending `extra` characters reallocate the buffer?
    pub fn would_grow(&self, extra: usize) -> bool {
        if self.is_wide {
            self.wide.len() + extra > self.wide.capacity()
        } else {
            self.narrow.len() + extra > self.narrow.capacity()
        }
    }
    #[cold]
    fn widen(&mut self, extra: usize) {
        self.wide.reserve(self.narrow.len() + extra.max(16));
        self.wide.extend(self.narrow.iter().map(|&b| b as u32));
        self.narrow = Vec::new();
        self.is_wide = true;
    }
    #[inline]
    pub fn push(&mut self, c: u32) {
        self.maxc = self.maxc.max(c);
        if !self.is_wide {
            if c < 0x100 {
                self.narrow.push(c as u8);
                return;
            }
            self.widen(1);
        }
        self.wide.push(c);
    }
    /// Append ASCII bytes (no check that they are ASCII beyond a debug assertion).
    #[inline]
    pub fn push_ascii(&mut self, s: &[u8]) {
        debug_assert!(s.is_ascii());
        if !self.is_wide {
            self.narrow.extend_from_slice(s);
        } else {
            self.wide.extend(s.iter().map(|&b| b as u32));
        }
        // ASCII never raises `maxc` past the ASCII/Latin-1 boundary that matters
    }
    pub fn push_str(&mut self, s: &str) {
        if s.is_ascii() {
            self.push_ascii(s.as_bytes());
            return;
        }
        for c in s.chars() {
            self.push(c as u32);
        }
    }
    pub fn push_data(&mut self, d: StrData) {
        match d {
            StrData::L1(b) if !self.is_wide => {
                self.narrow.extend_from_slice(b);
                if let Some(m) = b.iter().copied().max() {
                    self.maxc = self.maxc.max(m as u32);
                }
            }
            _ => {
                for c in d.iter() {
                    self.push(c);
                }
            }
        }
    }
    pub fn push_obj(&mut self, o: &PyObject) {
        let m = max_char(o).min(0x10ffff);
        let d = str_data(o);
        if !self.is_wide {
            if let StrData::L1(b) = d {
                self.narrow.extend_from_slice(b);
                self.maxc = self.maxc.max(m);
                return;
            }
            self.widen(d.len());
        }
        self.maxc = self.maxc.max(m);
        match d {
            StrData::U4(u) => self.wide.extend_from_slice(u),
            _ => self.wide.extend(d.iter()),
        }
    }
    pub fn len(&self) -> usize {
        if self.is_wide { self.wide.len() } else { self.narrow.len() }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn finish(self) -> PyRef {
        if !self.is_wide {
            let b = &self.narrow;
            if b.len() <= 1 {
                return if b.is_empty() { empty_str() } else { latin1_char(b[0]) };
            }
            let r = alloc_str(b.len(), if self.maxc < 0x80 { 0x7f } else { 0xff });
            unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), str_data_ptr(&r), b.len()) };
            return r;
        }
        let real_max = self.wide.iter().copied().max().unwrap_or(0);
        from_codepoints_max(&self.wide, real_max)
    }
}

// ---------------- interning ----------------

use std::sync::Mutex;

/// Interned strs: an open-addressing set of object pointers keyed on their (cached)
/// hash, linear probing, load factor <= 1/2 (perf2: one flat array instead of a hash
/// map of per-hash vectors — no allocation per interned string).
struct InternTable {
    slots: Vec<usize>,
    len: usize,
}

impl InternTable {
    /// Slot index holding a str equal to the probe (`eq`), or the empty slot to use.
    #[inline]
    fn find(&self, h: isize, eq: impl Fn(&PyObject) -> bool) -> Result<usize, usize> {
        let mask = self.slots.len() - 1;
        let mut i = (h as usize ^ (h as usize >> 29)) & mask;
        loop {
            let p = self.slots[i];
            if p == 0 {
                return Err(i);
            }
            let o = unsafe { &*(p as *mut PyObject) };
            if ascii_obj(o).hash == h && eq(o) {
                return Ok(i);
            }
            i = (i + 1) & mask;
        }
    }
    fn insert_at(&mut self, i: usize, p: usize) {
        self.slots[i] = p;
        self.len += 1;
        if self.len * 2 > self.slots.len() {
            let n2 = self.slots.len() * 2;
            let old = std::mem::replace(&mut self.slots, vec![0; n2]);
            let mask = self.slots.len() - 1;
            for q in old.into_iter().filter(|&q| q != 0) {
                let h = ascii_obj(unsafe { &*(q as *mut PyObject) }).hash;
                let mut j = (h as usize ^ (h as usize >> 29)) & mask;
                while self.slots[j] != 0 {
                    j = (j + 1) & mask;
                }
                self.slots[j] = q;
            }
        }
    }
}

static INTERNED: Mutex<InternTable> = Mutex::new(InternTable { slots: Vec::new(), len: 0 });

#[inline]
fn interned_table() -> std::sync::MutexGuard<'static, InternTable> {
    let mut g = INTERNED.lock().unwrap_or_else(|e| e.into_inner());
    if g.slots.is_empty() {
        g.slots = vec![0; 8192];
    }
    g
}

/// Make `s` the canonical interned instance (caller holds the table lock and checked
/// that no equal str is interned).
unsafe fn adopt_interned(t: &mut InternTable, slot: usize, s: &PyRef) {
    unsafe {
        make_immortal(s.as_ptr());
        // SSTATE_INTERNED_IMMORTAL; one-character Latin-1 strings are the process-wide
        // singletons and report SSTATE_INTERNED_IMMORTAL_STATIC (capi2)
        let single = str_len(s) == 1 && str_kind(s) == 1;
        (*(s.as_ptr() as *mut PyASCIIObject)).state |= if single { 3 } else { 2 };
    }
    t.insert_at(slot, s.as_ptr() as usize);
}

/// Intern a str object; returns the canonical (immortal) instance.
pub fn intern(s: PyRef) -> PyRef {
    if !is_str_exact(&s) {
        return s;
    }
    if ascii_obj(&s).state & 0xff != 0 {
        return s;
    }
    let key = str_hash(&s);
    let mut t = interned_table();
    match t.find(key, |o| str_eq(o, &s)) {
        Ok(i) => unsafe { PyRef::from_borrowed(t.slots[i] as *mut PyObject) },
        Err(i) => {
            unsafe { adopt_interned(&mut t, i, &s) };
            s
        }
    }
}

/// Interned str with the given Latin-1 content (marshal, names in code objects): an
/// already interned instance is returned without allocating a new object.
pub fn intern_latin1(b: &[u8]) -> PyRef {
    let h = crate::hash::hash_bytes(b);
    let eq = |o: &PyObject| str_kind(o) == 1 && str_len(o) == b.len() && unsafe { std::slice::from_raw_parts(str_data_ptr(o), b.len()) } == b;
    {
        let t = interned_table();
        if let Ok(i) = t.find(h, eq) {
            return unsafe { PyRef::from_borrowed(t.slots[i] as *mut PyObject) };
        }
    }
    intern(from_latin1(b))
}

/// The interned instance equal to exact str `s`, without interning `s` itself
/// (perf-runtime: attribute-name lookups against shapes).
pub fn lookup_interned(s: &PyObject) -> Option<*mut PyObject> {
    if is_interned(s) {
        return Some(s.as_ptr());
    }
    if !is_str_exact(s) {
        return None;
    }
    let key = str_hash(s);
    let t = interned_table();
    t.find(key, |o| str_eq(o, s)).ok().map(|i| t.slots[i] as *mut PyObject)
}

pub fn intern_str(s: &str) -> PyRef {
    if s.is_ascii() {
        // (perf4) an already interned name is found without allocating a str
        return intern_latin1(s.as_bytes());
    }
    intern(new_str(s))
}

pub fn is_interned(o: &PyObject) -> bool {
    ascii_obj(o).state & 0xff != 0
}

/// `name!("__init__")` → `&'static PyObject` interned string, cached per call site.
#[macro_export]
macro_rules! name {
    ($s:expr) => {{
        static CELL: std::sync::atomic::AtomicPtr<$crate::ffi::PyObject> =
            std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
        let mut p = CELL.load(std::sync::atomic::Ordering::Acquire);
        if p.is_null() {
            p = $crate::types::str::intern_str($s).into_raw();
            CELL.store(p, std::sync::atomic::Ordering::Release);
        }
        unsafe { &*p }
    }};
}
