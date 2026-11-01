//! Case mapping (full mappings, Final_Sigma) and character-class predicates.

use super::writer::{data_class, dst_mut, DstMut, Writer};
use crate::ffi::PyObject;
use crate::object::*;
use crate::types::str::{self as pystr, StrData};
use rpy_unicode as ucd;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CaseOp {
    Lower,
    Upper,
    Title,
    Capitalize,
    Swapcase,
    Casefold,
}

/// Final_Sigma test at index i of d.
fn final_sigma(d: StrData, i: usize) -> bool {
    let before = (0..i).rev().map(move |j| d.get(j));
    let after = (i + 1..d.len()).map(move |j| d.get(j));
    ucd::is_final_sigma(before, after)
}

#[inline]
fn lower_at(d: StrData, i: usize, out: &mut [u32; 3]) -> usize {
    let c = d.get(i);
    if c == 0x3A3 {
        out[0] = if final_sigma(d, i) { 0x3C2 } else { 0x3C3 };
        1
    } else {
        ucd::to_lower_full(c, out)
    }
}

/// ASCII fast path: same length, in place mapping into a new ascii string.
fn ascii_map(s: &[u8], op: CaseOp) -> PyRef {
    let r = pystr::alloc_str(s.len(), 0x7f);
    if let DstMut::L1(d) = dst_mut(&r) {
        match op {
            CaseOp::Lower | CaseOp::Casefold => {
                for (o, &c) in d.iter_mut().zip(s) {
                    *o = c.to_ascii_lowercase();
                }
            }
            CaseOp::Upper => {
                for (o, &c) in d.iter_mut().zip(s) {
                    *o = c.to_ascii_uppercase();
                }
            }
            CaseOp::Swapcase => {
                for (o, &c) in d.iter_mut().zip(s) {
                    *o = if c.is_ascii_alphabetic() { c ^ 0x20 } else { c };
                }
            }
            CaseOp::Capitalize => {
                for (i, (o, &c)) in d.iter_mut().zip(s).enumerate() {
                    *o = if i == 0 { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() };
                }
            }
            CaseOp::Title => {
                let mut prev_cased = false;
                for (o, &c) in d.iter_mut().zip(s) {
                    let cased = c.is_ascii_alphabetic();
                    *o = if prev_cased { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() };
                    prev_cased = cased;
                }
            }
        }
    }
    r
}

pub fn case_map(o: &PyObject, op: CaseOp) -> PyRef {
    let d = pystr::str_data(o);
    let n = d.len();
    if n == 0 {
        return pystr::new_str("");
    }
    if let (true, StrData::L1(s)) = (pystr::str_is_ascii(o), d) {
        if n == 1 {
            let c = s[0];
            let m = match op {
                CaseOp::Lower | CaseOp::Casefold => c.to_ascii_lowercase(),
                CaseOp::Upper | CaseOp::Capitalize | CaseOp::Title => c.to_ascii_uppercase(),
                CaseOp::Swapcase => {
                    if c.is_ascii_alphabetic() {
                        c ^ 0x20
                    } else {
                        c
                    }
                }
            };
            return pystr::latin1_char(m);
        }
        return ascii_map(s, op);
    }
    // Latin-1 lower(): stays Latin-1 and same length, non-ascii stays non-ascii.
    if let (CaseOp::Lower, StrData::L1(s)) = (op, d) {
        let r = pystr::alloc_str(n, 0xff);
        if let DstMut::L1(dd) = dst_mut(&r) {
            for (o, &c) in dd.iter_mut().zip(s) {
                *o = ucd::to_lower_simple(c as u32) as u8;
            }
        }
        return fix_canonical(r);
    }
    let mut w = Writer::with_capacity(n, data_class(d));
    let mut buf = [0u32; 3];
    match op {
        CaseOp::Lower => {
            for i in 0..n {
                let k = lower_at(d, i, &mut buf);
                for &c in &buf[..k] {
                    w.push(c);
                }
            }
        }
        CaseOp::Upper => {
            for c in d.iter() {
                let k = ucd::to_upper_full(c, &mut buf);
                for &c in &buf[..k] {
                    w.push(c);
                }
            }
        }
        CaseOp::Casefold => {
            for c in d.iter() {
                let k = ucd::casefold_full(c, &mut buf);
                for &c in &buf[..k] {
                    w.push(c);
                }
            }
        }
        CaseOp::Swapcase => {
            for i in 0..n {
                let c = d.get(i);
                let k = if ucd::is_uppercase(c) {
                    lower_at(d, i, &mut buf)
                } else if ucd::is_lowercase(c) {
                    ucd::to_upper_full(c, &mut buf)
                } else {
                    buf[0] = c;
                    1
                };
                for &c in &buf[..k] {
                    w.push(c);
                }
            }
        }
        CaseOp::Capitalize => {
            for i in 0..n {
                let k = if i == 0 { ucd::to_title_full(d.get(0), &mut buf) } else { lower_at(d, i, &mut buf) };
                for &c in &buf[..k] {
                    w.push(c);
                }
            }
        }
        CaseOp::Title => {
            let mut prev_cased = false;
            for i in 0..n {
                let c = d.get(i);
                let k = if prev_cased { lower_at(d, i, &mut buf) } else { ucd::to_title_full(c, &mut buf) };
                prev_cased = ucd::is_cased(c);
                for &c in &buf[..k] {
                    w.push(c);
                }
            }
        }
    }
    w.finish()
}

/// Re-canonicalise a Latin-1 result that might be all-ASCII.
fn fix_canonical(r: PyRef) -> PyRef {
    let d = pystr::str_data(&r);
    if let StrData::L1(s) = d {
        if s.is_ascii() {
            return super::writer::new_from_data(d);
        }
    }
    r
}

// ------------------------------------------------------------------ predicates

#[derive(Clone, Copy)]
pub enum Pred {
    Alnum,
    Alpha,
    Decimal,
    Digit,
    Numeric,
    Space,
    Printable,
}

#[inline]
fn pred_char(p: Pred, c: u32) -> bool {
    match p {
        Pred::Alnum => ucd::is_alnum(c),
        Pred::Alpha => ucd::is_alpha(c),
        Pred::Decimal => ucd::is_decimal(c),
        Pred::Digit => ucd::is_digit(c),
        Pred::Numeric => ucd::is_numeric(c),
        Pred::Space => ucd::is_space(c),
        Pred::Printable => ucd::is_printable(c),
    }
}

#[inline]
fn pred_ascii(p: Pred, c: u8) -> bool {
    match p {
        Pred::Alnum => c.is_ascii_alphanumeric(),
        Pred::Alpha => c.is_ascii_alphabetic(),
        Pred::Decimal | Pred::Digit | Pred::Numeric => c.is_ascii_digit(),
        Pred::Space => matches!(c, b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | 0x1c..=0x1f | b' '),
        Pred::Printable => (0x20..0x7f).contains(&c),
    }
}

pub fn predicate(o: &PyObject, p: Pred) -> bool {
    let d = pystr::str_data(o);
    if d.len() == 0 {
        return matches!(p, Pred::Printable);
    }
    if let (true, StrData::L1(s)) = (pystr::str_is_ascii(o), d) {
        return s.iter().all(|&c| pred_ascii(p, c));
    }
    d.iter().all(|c| pred_char(p, c))
}

pub fn islower(o: &PyObject) -> bool {
    let d = pystr::str_data(o);
    if let (true, StrData::L1(s)) = (pystr::str_is_ascii(o), d) {
        let mut cased = false;
        for &c in s {
            if c.is_ascii_uppercase() {
                return false;
            }
            cased |= c.is_ascii_lowercase();
        }
        return cased;
    }
    if d.len() == 1 {
        return ucd::is_lowercase(d.get(0));
    }
    let mut cased = false;
    for c in d.iter() {
        if ucd::is_uppercase(c) || ucd::is_titlecase(c) {
            return false;
        }
        if !cased && ucd::is_lowercase(c) {
            cased = true;
        }
    }
    cased
}

pub fn isupper(o: &PyObject) -> bool {
    let d = pystr::str_data(o);
    if let (true, StrData::L1(s)) = (pystr::str_is_ascii(o), d) {
        let mut cased = false;
        for &c in s {
            if c.is_ascii_lowercase() {
                return false;
            }
            cased |= c.is_ascii_uppercase();
        }
        return cased;
    }
    if d.len() == 1 {
        return ucd::is_uppercase(d.get(0));
    }
    let mut cased = false;
    for c in d.iter() {
        if ucd::is_lowercase(c) || ucd::is_titlecase(c) {
            return false;
        }
        if !cased && ucd::is_uppercase(c) {
            cased = true;
        }
    }
    cased
}

pub fn istitle(o: &PyObject) -> bool {
    let d = pystr::str_data(o);
    if d.len() == 1 {
        let c = d.get(0);
        return ucd::is_titlecase(c) || ucd::is_uppercase(c);
    }
    let mut cased = false;
    let mut prev_cased = false;
    for c in d.iter() {
        if ucd::is_uppercase(c) || ucd::is_titlecase(c) {
            if prev_cased {
                return false;
            }
            prev_cased = true;
            cased = true;
        } else if ucd::is_lowercase(c) {
            if !prev_cased {
                return false;
            }
            prev_cased = true;
            cased = true;
        } else {
            prev_cased = false;
        }
    }
    cased
}

pub fn isidentifier(o: &PyObject) -> bool {
    let d = pystr::str_data(o);
    if d.len() == 0 {
        return false;
    }
    let first = d.get(0);
    if !(first == '_' as u32 || ucd::is_xid_start(first)) {
        return false;
    }
    (1..d.len()).all(|i| ucd::is_xid_continue(d.get(i)))
}

/// Whitespace test used by split/strip (Py_UNICODE_ISSPACE).
#[inline]
pub fn is_space(c: u32) -> bool {
    if c < 128 {
        matches!(c, 0x09..=0x0d | 0x1c..=0x20)
    } else {
        ucd::is_space(c)
    }
}

/// Line-break test used by splitlines.
#[inline]
pub fn is_linebreak(c: u32) -> bool {
    match c {
        0x0a | 0x0b | 0x0c | 0x0d | 0x1c | 0x1d | 0x1e | 0x85 | 0x2028 | 0x2029 => true,
        _ => false,
    }
}
