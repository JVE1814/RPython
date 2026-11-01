//! `unicodedata.UCD` objects: the current database and `ucd_3_2_0`.

use crate::names::{self, Lookup};
use crate::normalize::{is_normalized_impl, normalize_impl, Form};
use crate::tables::db::{BIDI_NAMES, CATEGORY_NAMES, EAW_NAMES};
use crate::tables::v320::{ASSIGNED, CHANGES, CHANGE_RECORDS};
use crate::{db_record, ChangeRecord};

/// A Unicode database version, mirroring CPython's `unicodedata.UCD`
/// objects: `Ucd::Current` is the `unicodedata` module itself (Unicode
/// [`crate::UNIDATA_VERSION`]), `Ucd::V3_2_0` is `unicodedata.ucd_3_2_0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ucd {
    Current,
    V3_2_0,
}

/// Is `c` assigned in Unicode 3.2.0?
pub(crate) fn assigned_320(c: u32) -> bool {
    let i = ASSIGNED.partition_point(|&(_, end)| end < c);
    matches!(ASSIGNED.get(i), Some(&(start, _)) if start <= c)
}

fn change_320(c: u32) -> Option<&'static ChangeRecord> {
    CHANGES
        .binary_search_by_key(&c, |&(cp, _)| cp)
        .ok()
        .map(|i| &CHANGE_RECORDS[CHANGES[i].1 as usize])
}

/// State of a character in the 3.2.0 database.
enum Old {
    /// Not assigned in 3.2.0.
    Unassigned,
    /// Assigned, with these 3.2.0 property values.
    Changed(&'static ChangeRecord),
    /// Assigned, properties as in the current database.
    Same,
}

fn old(c: u32) -> Old {
    if !assigned_320(c) {
        Old::Unassigned
    } else if let Some(r) = change_320(c) {
        Old::Changed(r)
    } else {
        Old::Same
    }
}

impl Ucd {
    /// `unidata_version` attribute.
    pub fn unidata_version(self) -> &'static str {
        match self {
            Ucd::Current => crate::UNIDATA_VERSION,
            Ucd::V3_2_0 => "3.2.0",
        }
    }

    /// `category(chr)`.
    pub fn category(self, c: u32) -> &'static str {
        if self == Ucd::V3_2_0 {
            match old(c) {
                Old::Unassigned => return "Cn",
                Old::Changed(r) => return CATEGORY_NAMES[r.category as usize],
                Old::Same => {}
            }
        }
        crate::category(c)
    }

    /// `bidirectional(chr)`; "" for characters unassigned in 3.2.0.
    pub fn bidirectional(self, c: u32) -> &'static str {
        if self == Ucd::V3_2_0 {
            match old(c) {
                Old::Unassigned => return "",
                Old::Changed(r) => return BIDI_NAMES[r.bidi as usize],
                Old::Same => {}
            }
        }
        crate::bidirectional(c)
    }

    /// `combining(chr)`.
    pub fn combining(self, c: u32) -> u8 {
        if self == Ucd::V3_2_0 && !assigned_320(c) {
            return 0;
        }
        crate::combining(c)
    }

    /// `east_asian_width(chr)`.
    pub fn east_asian_width(self, c: u32) -> &'static str {
        if self == Ucd::V3_2_0 {
            match old(c) {
                Old::Unassigned => {
                    // characters assigned since 3.2.0 report "N"; code points
                    // unassigned in both versions keep their current default.
                    if db_record(c).category != 0 {
                        return "N";
                    }
                }
                Old::Changed(r) => return EAW_NAMES[r.eaw as usize],
                Old::Same => {}
            }
        }
        crate::east_asian_width(c)
    }

    /// `mirrored(chr)`.
    pub fn mirrored(self, c: u32) -> bool {
        if self == Ucd::V3_2_0 {
            match old(c) {
                Old::Unassigned => return false,
                Old::Changed(r) => return r.mirrored,
                Old::Same => {}
            }
        }
        crate::mirrored(c)
    }

    /// `decimal(chr)`; None means "raise ValueError / return default".
    pub fn decimal(self, c: u32) -> Option<u32> {
        if self == Ucd::V3_2_0 {
            match old(c) {
                Old::Unassigned => return None,
                Old::Changed(r) => return if r.decimal == 0xFF { None } else { Some(r.decimal as u32) },
                Old::Same => {}
            }
        }
        crate::to_decimal(c)
    }

    /// `digit(chr)` (CPython does not consult 3.2.0 data for this one).
    pub fn digit(self, c: u32) -> Option<u32> {
        crate::to_digit(c)
    }

    /// `numeric(chr)`.
    pub fn numeric(self, c: u32) -> Option<f64> {
        if self == Ucd::V3_2_0 {
            match old(c) {
                Old::Unassigned => return None,
                Old::Changed(r) => return if r.numeric.is_nan() { None } else { Some(r.numeric) },
                Old::Same => {}
            }
        }
        crate::to_numeric(c)
    }

    /// `decomposition(chr)`, e.g. "<compat> 0020 0301".
    pub fn decomposition(self, c: u32) -> String {
        if self == Ucd::V3_2_0 && !assigned_320(c) {
            return String::new();
        }
        crate::decomposition(c)
    }

    /// `name(chr)`; None means no name (ValueError / default).
    pub fn name(self, c: u32) -> Option<String> {
        if self == Ucd::V3_2_0 && !assigned_320(c) {
            return None;
        }
        names::name(c)
    }

    /// Appends the name to `out`; returns false if there is none.
    pub fn name_into(self, c: u32, out: &mut String) -> bool {
        if self == Ucd::V3_2_0 && !assigned_320(c) {
            return false;
        }
        names::name_into(c, out)
    }

    /// `lookup(name)`: case-insensitive; the current database also accepts
    /// name aliases and named sequences, 3.2.0 accepts neither.
    pub fn lookup(self, name: &str) -> Option<Lookup> {
        match self {
            Ucd::Current => names::lookup_full(name),
            Ucd::V3_2_0 => names::lookup_plain(name).filter(|&c| assigned_320(c)).map(Lookup::Char),
        }
    }

    /// `normalize(form, unistr)`.
    pub fn normalize(self, form: Form, s: &[u32]) -> Vec<u32> {
        normalize_impl(form, s, self == Ucd::V3_2_0)
    }

    /// `is_normalized(form, unistr)`.
    pub fn is_normalized(self, form: Form, s: &[u32]) -> bool {
        is_normalized_impl(form, s, self == Ucd::V3_2_0)
    }

    /// `isxidstart(chr)` (current data for both versions).
    pub fn isxidstart(self, c: u32) -> bool {
        crate::is_xid_start(c)
    }

    /// `isxidcontinue(chr)`.
    pub fn isxidcontinue(self, c: u32) -> bool {
        crate::is_xid_continue(c)
    }
}
