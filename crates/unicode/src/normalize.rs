//! Unicode normalization (NFC, NFD, NFKC, NFKD) for the current database and
//! for Unicode 3.2.0 (`unicodedata.ucd_3_2_0`, used by IDNA / stringprep).

use crate::tables::db::COMP_PAIRS;
use crate::{
    combining, db_record, raw_decomposition, L_BASE, L_COUNT, NFC_MAYBE, NFC_NO, NFD_NO,
    NFKC_MAYBE, NFKC_NO, NFKD_NO, N_COUNT, S_BASE, S_COUNT, T_BASE, T_COUNT, V_BASE, V_COUNT,
};

/// A normalization form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Form {
    NFC,
    NFD,
    NFKC,
    NFKD,
}

impl Form {
    /// Parses "NFC", "NFD", "NFKC" or "NFKD" (exact, as CPython).
    pub fn from_name(s: &str) -> Option<Form> {
        match s {
            "NFC" => Some(Form::NFC),
            "NFD" => Some(Form::NFD),
            "NFKC" => Some(Form::NFKC),
            "NFKD" => Some(Form::NFKD),
            _ => None,
        }
    }

    #[inline]
    fn compat(self) -> bool {
        matches!(self, Form::NFKC | Form::NFKD)
    }

    #[inline]
    fn composed(self) -> bool {
        matches!(self, Form::NFC | Form::NFKC)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Qc {
    Yes,
    No,
    Maybe,
}

/// Quick check (current database only).
fn quick_check(form: Form, s: &[u32]) -> Qc {
    let (no, maybe) = match form {
        Form::NFC => (NFC_NO, NFC_MAYBE),
        Form::NFKC => (NFKC_NO, NFKC_MAYBE),
        Form::NFD => (NFD_NO, 0),
        Form::NFKD => (NFKD_NO, 0),
    };
    let mut result = Qc::Yes;
    let mut last = 0u8;
    for &c in s {
        if c < 0xA0 {
            // ccc=0 and quick check Yes for every form
            last = 0;
            continue;
        }
        let r = db_record(c);
        let ccc = r.combining;
        if ccc != 0 && last > ccc {
            return Qc::No;
        }
        if r.flags & no != 0 {
            return Qc::No;
        }
        if r.flags & maybe != 0 {
            result = Qc::Maybe;
        }
        last = ccc;
    }
    result
}

#[inline]
fn assigned_320(c: u32) -> bool {
    crate::ucd::assigned_320(c)
}

fn correction_320(c: u32) -> Option<u32> {
    let t = &crate::tables::v320::NORMALIZATION_CORRECTIONS;
    t.binary_search_by_key(&c, |&(a, _)| a).ok().map(|i| t[i].1)
}

fn decompose_char(c: u32, compat: bool, v320: bool, out: &mut Vec<u32>) {
    if (S_BASE..S_BASE + S_COUNT).contains(&c) {
        let s = c - S_BASE;
        out.push(L_BASE + s / N_COUNT);
        out.push(V_BASE + (s % N_COUNT) / T_COUNT);
        let t = s % T_COUNT;
        if t != 0 {
            out.push(T_BASE + t);
        }
        return;
    }
    if v320 {
        if !assigned_320(c) {
            out.push(c);
            return;
        }
        if let Some(d) = correction_320(c) {
            decompose_char(d, compat, v320, out);
            return;
        }
    }
    match raw_decomposition(c) {
        Some((tag, seq)) if tag == 0 || compat => {
            for &d in seq {
                decompose_char(d, compat, v320, out);
            }
        }
        _ => out.push(c),
    }
}

/// Puts runs of non-starters into canonical order (stable sort by ccc).
fn canonical_order(v: &mut [u32]) {
    let mut i = 0;
    while i < v.len() {
        if combining(v[i]) == 0 {
            i += 1;
            continue;
        }
        let start = i;
        let mut sorted = true;
        let mut last = 0;
        while i < v.len() {
            let cc = combining(v[i]);
            if cc == 0 {
                break;
            }
            if cc < last {
                sorted = false;
            }
            last = cc;
            i += 1;
        }
        if !sorted {
            v[start..i].sort_by_key(|&c| combining(c));
        }
    }
}

fn decompose(s: &[u32], compat: bool, v320: bool) -> Vec<u32> {
    let mut out = Vec::with_capacity(s.len() + s.len() / 2);
    for &c in s {
        if c < 0xA0 {
            out.push(c);
        } else {
            decompose_char(c, compat, v320, &mut out);
        }
    }
    canonical_order(&mut out);
    out
}

/// Primary composite of a starter and a following character.
#[inline]
pub(crate) fn compose_pair(a: u32, b: u32) -> Option<u32> {
    // Hangul L + V
    if (L_BASE..L_BASE + L_COUNT).contains(&a) && (V_BASE..V_BASE + V_COUNT).contains(&b) {
        return Some(S_BASE + ((a - L_BASE) * V_COUNT + (b - V_BASE)) * T_COUNT);
    }
    // Hangul LV + T
    if (S_BASE..S_BASE + S_COUNT).contains(&a)
        && (a - S_BASE) % T_COUNT == 0
        && (T_BASE + 1..T_BASE + T_COUNT).contains(&b)
    {
        return Some(a + (b - T_BASE));
    }
    COMP_PAIRS
        .binary_search_by(|&(x, y, _)| (x, y).cmp(&(a, b)))
        .ok()
        .map(|i| COMP_PAIRS[i].2)
}

fn compose(v: Vec<u32>) -> Vec<u32> {
    let mut result: Vec<u32> = Vec::with_capacity(v.len());
    let mut starter: Option<usize> = None;
    // ccc of the last character appended after the starter (None: adjacent)
    let mut last_ccc: Option<u8> = None;
    for c in v {
        let ccc = combining(c);
        if let Some(si) = starter {
            let blocked = match last_ccc {
                None => false,
                Some(lc) => lc == 0 || lc >= ccc,
            };
            if !blocked {
                if let Some(p) = compose_pair(result[si], c) {
                    result[si] = p;
                    continue;
                }
            }
        }
        if ccc == 0 {
            starter = Some(result.len());
            last_ccc = None;
        } else {
            last_ccc = Some(ccc);
        }
        result.push(c);
    }
    result
}

pub(crate) fn normalize_impl(form: Form, s: &[u32], v320: bool) -> Vec<u32> {
    if !v320 && quick_check(form, s) == Qc::Yes {
        return s.to_vec();
    }
    let d = decompose(s, form.compat(), v320);
    if form.composed() {
        compose(d)
    } else {
        d
    }
}

pub(crate) fn is_normalized_impl(form: Form, s: &[u32], v320: bool) -> bool {
    if !v320 {
        match quick_check(form, s) {
            Qc::Yes => return true,
            Qc::No => return false,
            Qc::Maybe => {}
        }
    }
    normalize_impl(form, s, v320) == s
}

/// `unicodedata.normalize(form, s)` for the current database.
pub fn normalize(form: Form, s: &[u32]) -> Vec<u32> {
    normalize_impl(form, s, false)
}

/// `unicodedata.is_normalized(form, s)` for the current database.
pub fn is_normalized(form: Form, s: &[u32]) -> bool {
    is_normalized_impl(form, s, false)
}

pub(crate) fn normalize_if_needed_impl(form: Form, s: &[u32], v320: bool) -> Option<Vec<u32>> {
    if !v320 && quick_check(form, s) == Qc::Yes {
        return None;
    }
    let d = decompose(s, form.compat(), v320);
    let r = if form.composed() { compose(d) } else { d };
    if r == s {
        None
    } else {
        Some(r)
    }
}

/// Like [`normalize`] but returns `None` (no allocation in the common case)
/// when `s` is already in normal form.
pub fn normalize_if_needed(form: Form, s: &[u32]) -> Option<Vec<u32>> {
    normalize_if_needed_impl(form, s, false)
}
