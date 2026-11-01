//! Extended grapheme cluster segmentation (UAX #29) and the related
//! properties (`unicodedata.grapheme_cluster_break`, `indic_conjunct_break`,
//! `extended_pictographic`, `iter_graphemes`).

use crate::db_record;
use crate::tables::db::{GCB_NAMES, INCB_NAMES};
use crate::EXT_PICT;

// indices into GCB_NAMES (must match gen)
#[allow(dead_code)]
const OTHER: u8 = 0;
const PREPEND: u8 = 1;
const CR: u8 = 2;
const LF: u8 = 3;
const CONTROL: u8 = 4;
const EXTEND: u8 = 5;
const RI: u8 = 6;
const SPACING_MARK: u8 = 7;
const L: u8 = 8;
const V: u8 = 9;
const T: u8 = 10;
const LV: u8 = 11;
const LVT: u8 = 12;
const ZWJ: u8 = 13;

// indices into INCB_NAMES
const INCB_LINKER: u8 = 1;
const INCB_CONSONANT: u8 = 2;
const INCB_EXTEND: u8 = 3;

/// Grapheme_Cluster_Break property value name, e.g. "Extend".
pub fn grapheme_cluster_break(c: u32) -> &'static str {
    GCB_NAMES[db_record(c).gcb as usize]
}

/// Indic_Conjunct_Break property value name ("None", "Linker", "Consonant", "Extend").
pub fn indic_conjunct_break(c: u32) -> &'static str {
    INCB_NAMES[db_record(c).incb as usize]
}

/// Extended_Pictographic property.
pub fn extended_pictographic(c: u32) -> bool {
    db_record(c).flags & EXT_PICT != 0
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Emoji {
    None,
    Pict,    // ExtPict Extend*
    PictZwj, // ExtPict Extend* ZWJ
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Conj {
    None,
    Consonant,       // Consonant [Extend Linker]* (no linker yet)
    ConsonantLinker, // Consonant [Extend Linker]* Linker [Extend Linker]*
}

/// Returns the end of the extended grapheme cluster starting at `start`,
/// treating `start..end` as the whole text.  `get(i)` returns the code point
/// at index `i`.  Requires `start < end`.
pub fn next_grapheme_break_by(get: impl Fn(usize) -> u32, start: usize, end: usize) -> usize {
    debug_assert!(start < end);
    let c = get(start);
    let r = db_record(c);
    let mut prev = r.gcb;
    let mut ri_count = (prev == RI) as u32;
    let mut emoji = if r.flags & EXT_PICT != 0 { Emoji::Pict } else { Emoji::None };
    let mut conj = if r.incb == INCB_CONSONANT { Conj::Consonant } else { Conj::None };
    let mut i = start + 1;
    while i < end {
        let c = get(i);
        let r = db_record(c);
        let cur = r.gcb;
        let pict = r.flags & EXT_PICT != 0;
        let brk = if prev == CR && cur == LF {
            false // GB3
        } else if matches!(prev, CONTROL | CR | LF) || matches!(cur, CONTROL | CR | LF) {
            true // GB4, GB5
        } else if prev == L && matches!(cur, L | V | LV | LVT) {
            false // GB6
        } else if matches!(prev, LV | V) && matches!(cur, V | T) {
            false // GB7
        } else if matches!(prev, LVT | T) && cur == T {
            false // GB8
        } else if matches!(cur, EXTEND | ZWJ | SPACING_MARK) || prev == PREPEND {
            false // GB9, GB9a, GB9b
        } else if r.incb == INCB_CONSONANT && conj == Conj::ConsonantLinker {
            false // GB9c
        } else if prev == ZWJ && pict && emoji == Emoji::PictZwj {
            false // GB11
        } else {
            // GB12, GB13 / GB999
            !(prev == RI && cur == RI && ri_count % 2 == 1)
        };
        if brk {
            break;
        }
        // update state
        ri_count = if cur == RI { ri_count + 1 } else { 0 };
        emoji = if pict {
            Emoji::Pict
        } else if cur == EXTEND && emoji == Emoji::Pict {
            Emoji::Pict
        } else if cur == ZWJ && emoji == Emoji::Pict {
            Emoji::PictZwj
        } else {
            Emoji::None
        };
        conj = match r.incb {
            INCB_CONSONANT => Conj::Consonant,
            INCB_LINKER if conj != Conj::None => Conj::ConsonantLinker,
            INCB_EXTEND if conj != Conj::None => conj,
            _ => Conj::None,
        };
        prev = cur;
        i += 1;
    }
    i
}

/// [`next_grapheme_break_by`] on a slice.
pub fn next_grapheme_break(s: &[u32], start: usize, end: usize) -> usize {
    next_grapheme_break_by(|i| s[i], start, end)
}

/// Iterator over `(start, end)` index pairs of the grapheme clusters of a
/// slice (the core of `unicodedata.iter_graphemes`).
pub struct Graphemes<'a> {
    s: &'a [u32],
    pos: usize,
    end: usize,
}

impl<'a> Graphemes<'a> {
    /// Clusters of `s[start..end]` (indices are into `s`).
    pub fn new(s: &'a [u32], start: usize, end: usize) -> Self {
        let end = end.min(s.len());
        Graphemes { s, pos: start.min(end), end }
    }
}

impl Iterator for Graphemes<'_> {
    type Item = (usize, usize);
    fn next(&mut self) -> Option<(usize, usize)> {
        if self.pos >= self.end {
            return None;
        }
        let start = self.pos;
        self.pos = next_grapheme_break(self.s, start, self.end);
        Some((start, self.pos))
    }
}
