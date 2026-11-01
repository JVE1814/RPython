//! Width-adaptive string writer and exact-size string construction helpers.
//!
//! `Writer` stores code units in the narrowest PEP 393 width able to hold
//! everything written so far, widening on demand; `finish` produces a
//! canonical compact str.  The `new_*` helpers allocate the result object
//! directly when the final length and width are known.

use super::search::Unit;
use crate::ffi::PyObject;
use crate::object::*;
use crate::types::str::{self as pystr, StrData};

pub enum Buf {
    L1(Vec<u8>),
    U2(Vec<u16>),
    U4(Vec<u32>),
}

pub struct Writer {
    pub buf: Buf,
    pub maxc: u32,
}

#[inline(always)]
pub fn kind_for(maxc: u32) -> u32 {
    if maxc < 0x100 {
        1
    } else if maxc < 0x10000 {
        2
    } else {
        4
    }
}

impl Writer {
    pub fn new() -> Self {
        Writer { buf: Buf::L1(Vec::new()), maxc: 0 }
    }
    pub fn with_capacity(n: usize, maxc_hint: u32) -> Self {
        let buf = match kind_for(maxc_hint) {
            1 => Buf::L1(Vec::with_capacity(n)),
            2 => Buf::U2(Vec::with_capacity(n)),
            _ => Buf::U4(Vec::with_capacity(n)),
        };
        Writer { buf, maxc: 0 }
    }
    #[inline]
    pub fn len(&self) -> usize {
        match &self.buf {
            Buf::L1(v) => v.len(),
            Buf::U2(v) => v.len(),
            Buf::U4(v) => v.len(),
        }
    }
    #[cold]
    fn widen(&mut self, to: u32) {
        let cur = match &self.buf {
            Buf::L1(_) => 1,
            Buf::U2(_) => 2,
            Buf::U4(_) => 4,
        };
        if to <= cur {
            return;
        }
        let old = std::mem::replace(&mut self.buf, Buf::L1(Vec::new()));
        self.buf = match (old, to) {
            (Buf::L1(v), 2) => {
                let mut n = Vec::with_capacity(v.capacity().max(8));
                n.extend(v.iter().map(|&c| c as u16));
                Buf::U2(n)
            }
            (Buf::L1(v), _) => {
                let mut n = Vec::with_capacity(v.capacity().max(8));
                n.extend(v.iter().map(|&c| c as u32));
                Buf::U4(n)
            }
            (Buf::U2(v), _) => {
                let mut n = Vec::with_capacity(v.capacity().max(8));
                n.extend(v.iter().map(|&c| c as u32));
                Buf::U4(n)
            }
            (b, _) => b,
        };
    }
    #[inline]
    fn note_max(&mut self, c: u32) {
        if c > self.maxc {
            let old = kind_for(self.maxc);
            self.maxc = c;
            let k = kind_for(c);
            if k > old {
                self.widen(k);
            }
        }
    }
    #[inline]
    pub fn push(&mut self, c: u32) {
        self.note_max(c);
        match &mut self.buf {
            Buf::L1(v) => v.push(c as u8),
            Buf::U2(v) => v.push(c as u16),
            Buf::U4(v) => v.push(c),
        }
    }
    pub fn push_ascii(&mut self, s: &str) {
        debug_assert!(s.is_ascii());
        if !s.is_empty() {
            self.note_max(s.bytes().max().unwrap() as u32);
        }
        match &mut self.buf {
            Buf::L1(v) => v.extend_from_slice(s.as_bytes()),
            Buf::U2(v) => v.extend(s.bytes().map(|b| b as u16)),
            Buf::U4(v) => v.extend(s.bytes().map(|b| b as u32)),
        }
    }
    pub fn push_str(&mut self, s: &str) {
        if s.is_ascii() {
            return self.push_ascii(s);
        }
        for c in s.chars() {
            self.push(c as u32);
        }
    }
    pub fn push_repeat(&mut self, c: u32, n: usize) {
        if n == 0 {
            return;
        }
        self.note_max(c);
        match &mut self.buf {
            Buf::L1(v) => v.resize(v.len() + n, c as u8),
            Buf::U2(v) => v.resize(v.len() + n, c as u16),
            Buf::U4(v) => v.resize(v.len() + n, c),
        }
    }
    /// Append a (possibly non-canonical) slice of code units; computes the
    /// exact maximum.
    pub fn push_data(&mut self, d: StrData) {
        if d.len() == 0 {
            return;
        }
        self.note_max(data_max(d));
        self.extend_data(d);
    }
    /// Append a whole canonical string object (cheap max bound).
    pub fn push_obj(&mut self, o: &PyObject) {
        if pystr::str_len(o) == 0 {
            return;
        }
        let m = pystr::max_char(o);
        if m > self.maxc {
            // a canonical string's bound gives the exact width/ascii class;
            // record a representative value of that class
            let rep = match m {
                0x7f => 0x7f,
                0xff => 0x80,
                0xffff => 0x100,
                _ => 0x10000,
            };
            self.note_max(rep.max(self.maxc));
        }
        self.extend_data(pystr::str_data(o));
    }
    fn extend_data(&mut self, d: StrData) {
        match (&mut self.buf, d) {
            (Buf::L1(v), StrData::L1(s)) => v.extend_from_slice(s),
            (Buf::U2(v), StrData::U2(s)) => v.extend_from_slice(s),
            (Buf::U4(v), StrData::U4(s)) => v.extend_from_slice(s),
            (Buf::L1(v), s) => v.extend(s.iter().map(|c| c as u8)),
            (Buf::U2(v), StrData::L1(s)) => v.extend(s.iter().map(|&c| c as u16)),
            (Buf::U2(v), s) => v.extend(s.iter().map(|c| c as u16)),
            (Buf::U4(v), s) => v.extend(s.iter()),
        }
    }
    pub fn finish(self) -> PyRef {
        // `maxc` may be a class representative (see push_obj); only its
        // width/ascii class matters for the allocation.
        match self.buf {
            Buf::L1(v) => {
                if v.is_empty() {
                    return pystr::new_str("");
                }
                if v.len() == 1 {
                    return pystr::latin1_char(v[0]);
                }
                let r = pystr::alloc_str(v.len(), if self.maxc < 0x80 { 0x7f } else { 0xff });
                unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), pystr::str_data_ptr(&r), v.len()) };
                r
            }
            Buf::U2(v) if kind_for(self.maxc) < 2 => new_from_units(&v),
            Buf::U4(v) if kind_for(self.maxc) < 4 => new_from_units(&v),
            Buf::U2(v) => {
                let r = pystr::alloc_str(v.len(), 0xffff);
                unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), pystr::str_data_ptr(&r) as *mut u16, v.len()) };
                r
            }
            Buf::U4(v) => {
                let r = pystr::alloc_str(v.len(), 0x10ffff);
                unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), pystr::str_data_ptr(&r) as *mut u32, v.len()) };
                r
            }
        }
    }
}

/// Exact maximum code point of a slice.
pub fn data_max(d: StrData) -> u32 {
    match d {
        StrData::L1(s) => {
            if s.is_ascii() {
                s.iter().copied().max().unwrap_or(0) as u32
            } else {
                0xff.min(s.iter().copied().max().unwrap_or(0) as u32)
            }
        }
        StrData::U2(s) => s.iter().copied().max().unwrap_or(0) as u32,
        StrData::U4(s) => s.iter().copied().max().unwrap_or(0),
    }
}

/// Width class of a slice: 0x7f (ascii), 0xff, 0xffff or 0x10ffff.
pub fn data_class(d: StrData) -> u32 {
    match d {
        StrData::L1(s) => {
            if s.is_ascii() {
                0x7f
            } else {
                0xff
            }
        }
        _ => class_of(data_max(d)),
    }
}

#[inline]
pub fn class_of(m: u32) -> u32 {
    if m < 0x80 {
        0x7f
    } else if m < 0x100 {
        0xff
    } else if m < 0x10000 {
        0xffff
    } else {
        0x10ffff
    }
}

/// Fallible allocation (MemoryError instead of abort for huge sizes).
pub fn try_alloc(len: usize, maxc: u32) -> crate::errors::PyResult<PyRef> {
    let k = kind_for(maxc) as usize;
    if len > (isize::MAX as usize) / k - 64 {
        return Err(crate::errors::memory_error());
    }
    // probe the allocator first for very large requests
    if len * k > (1usize << 32) {
        unsafe {
            let p = libc::malloc(len * k + 128);
            if p.is_null() {
                return Err(crate::errors::memory_error());
            }
            libc::free(p);
        }
    }
    Ok(pystr::alloc_str(len, maxc))
}

/// Mutable typed view of a freshly allocated string's data.
pub enum DstMut<'a> {
    L1(&'a mut [u8]),
    U2(&'a mut [u16]),
    U4(&'a mut [u32]),
}

pub fn dst_mut(r: &PyObject) -> DstMut<'_> {
    let n = pystr::str_len(r);
    let p = pystr::str_data_ptr(r);
    unsafe {
        match pystr::str_kind(r) {
            1 => DstMut::L1(std::slice::from_raw_parts_mut(p, n)),
            2 => DstMut::U2(std::slice::from_raw_parts_mut(p as *mut u16, n)),
            _ => DstMut::U4(std::slice::from_raw_parts_mut(p as *mut u32, n)),
        }
    }
}

/// Copy `src` into `dst[at..]` converting widths (dst must be wide enough).
pub fn copy_into(dst: &mut DstMut, at: usize, src: StrData) {
    match (dst, src) {
        (DstMut::L1(d), StrData::L1(s)) => d[at..at + s.len()].copy_from_slice(s),
        (DstMut::U2(d), StrData::U2(s)) => d[at..at + s.len()].copy_from_slice(s),
        (DstMut::U4(d), StrData::U4(s)) => d[at..at + s.len()].copy_from_slice(s),
        (DstMut::L1(d), s) => {
            for (i, c) in s.iter().enumerate() {
                d[at + i] = c as u8;
            }
        }
        (DstMut::U2(d), StrData::L1(s)) => {
            for (i, &c) in s.iter().enumerate() {
                d[at + i] = c as u16;
            }
        }
        (DstMut::U2(d), s) => {
            for (i, c) in s.iter().enumerate() {
                d[at + i] = c as u16;
            }
        }
        (DstMut::U4(d), s) => {
            for (i, c) in s.iter().enumerate() {
                d[at + i] = c;
            }
        }
    }
}

pub fn fill(dst: &mut DstMut, at: usize, n: usize, c: u32) {
    match dst {
        DstMut::L1(d) => d[at..at + n].fill(c as u8),
        DstMut::U2(d) => d[at..at + n].fill(c as u16),
        DstMut::U4(d) => d[at..at + n].fill(c),
    }
}

/// New canonical string from a slice of units (computes the exact max).
pub fn new_from_units<T: Unit>(s: &[T]) -> PyRef {
    let w = std::mem::size_of::<T>();
    let data = unsafe {
        match w {
            1 => StrData::L1(std::slice::from_raw_parts(s.as_ptr() as *const u8, s.len())),
            2 => StrData::U2(std::slice::from_raw_parts(s.as_ptr() as *const u16, s.len())),
            _ => StrData::U4(std::slice::from_raw_parts(s.as_ptr() as *const u32, s.len())),
        }
    };
    new_from_data(data)
}

/// New canonical string from a (possibly non-canonical) slice.
pub fn new_from_data(d: StrData) -> PyRef {
    let n = d.len();
    if n == 0 {
        return pystr::new_str("");
    }
    if n == 1 {
        let c = d.get(0);
        if c < 256 {
            return pystr::latin1_char(c as u8);
        }
    }
    let m = data_class(d);
    let r = pystr::alloc_str(n, m);
    copy_into(&mut dst_mut(&r), 0, d);
    r
}

/// Substring `o[start..end]` (returns `o` itself for the full range of an exact str).
pub fn substring(o: &PyObject, start: usize, end: usize) -> PyRef {
    let n = pystr::str_len(o);
    if start == 0 && end == n && pystr::is_str_exact(o) {
        return o.to_ref();
    }
    if start >= end {
        return pystr::new_str("");
    }
    let d = pystr::str_data(o);
    new_from_data(slice_data(d, start, end))
}

#[inline]
pub fn slice_data(d: StrData<'_>, start: usize, end: usize) -> StrData<'_> {
    match d {
        StrData::L1(s) => StrData::L1(&s[start..end]),
        StrData::U2(s) => StrData::U2(&s[start..end]),
        StrData::U4(s) => StrData::U4(&s[start..end]),
    }
}

/// Exact-str copy of `o` (for subclass instances), or `o` itself.
pub fn exact(o: &PyObject) -> PyRef {
    if pystr::is_str_exact(o) {
        o.to_ref()
    } else {
        new_from_data(pystr::str_data(o))
    }
}
