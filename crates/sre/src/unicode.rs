//! Character predicates and case mapping used by the regex engine.
//!
//! The engine never depends on a Unicode database crate.  Everything it needs
//! is reached through a [`UnicodeProvider`], a plain struct of function
//! pointers.  [`DEFAULT_PROVIDER`] is exact for ASCII/Latin-1 and uses Rust's
//! `char` methods for everything else; the runtime can plug in its own tables
//! (CPython-exact `unicodedata`) by building another `UnicodeProvider`.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Function-pointer table with every Unicode service the engine needs.
///
/// Semantics expected by the engine (mirroring CPython's `_sre`):
/// * `is_word(c)`  -- `str.isalnum() or c == '_'` for one character (`\w`).
/// * `is_digit(c)` -- `str.isdecimal()` (category Nd) (`\d`).
/// * `is_space(c)` -- `str.isspace()` (`\s`).
/// * `lower(c)` / `upper(c)` -- the *first* character of the full lower/upper
///   case mapping (this is what CPython's `_PyUnicode_ToLowercase` returns).
/// * `case_class(c)` -- all characters that match `c` under IGNORECASE
///   (sorted, including `c`), or an empty slice when `c` only matches itself.
///   Two characters `a`, `b` are equivalent iff `lower(a)` and `lower(b)` are
///   equal or are different lowercase characters sharing the same full
///   uppercase mapping (CPython's `_casefix` rule).
/// * `cased_chars()` -- sorted list of every character with a non-empty
///   `case_class`.
/// * `is_identifier_start/continue` -- for `str.isidentifier()` on group names.
/// * `is_alpha(c)` -- `str.isalpha()` (used to pick between two error messages).
/// * `is_printable(c)` -- `str.isprintable()` (used for `repr()` in messages).
/// * `lookup_name(name)` -- `unicodedata.lookup` for `\N{...}`; `None` when
///   the name is unknown or names a sequence.
/// * `locale_*` -- C-locale `tolower`/`toupper`/`isalnum` for LOCALE patterns;
///   called at *match* time so the current locale is honoured.
#[derive(Clone, Copy)]
pub struct UnicodeProvider {
    pub is_word: fn(u32) -> bool,
    pub is_digit: fn(u32) -> bool,
    pub is_space: fn(u32) -> bool,
    pub lower: fn(u32) -> u32,
    pub upper: fn(u32) -> u32,
    pub case_class: fn(u32) -> &'static [u32],
    pub cased_chars: fn() -> &'static [u32],
    pub is_identifier_start: fn(u32) -> bool,
    pub is_identifier_continue: fn(u32) -> bool,
    pub is_alpha: fn(u32) -> bool,
    pub is_printable: fn(u32) -> bool,
    pub lookup_name: fn(&str) -> Option<u32>,
    pub locale_lower: fn(u8) -> u8,
    pub locale_upper: fn(u8) -> u8,
    pub locale_is_alnum: fn(u8) -> bool,
}

impl std::fmt::Debug for UnicodeProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UnicodeProvider")
    }
}

/// The built-in provider (ASCII/Latin-1 exact, Rust `char` based elsewhere).
pub const DEFAULT_PROVIDER: UnicodeProvider = UnicodeProvider {
    is_word: default_is_word,
    is_digit: default_is_digit,
    is_space: default_is_space,
    lower: default_lower,
    upper: default_upper,
    case_class: default_case_class,
    cased_chars: default_cased_chars,
    is_identifier_start: default_id_start,
    is_identifier_continue: default_id_continue,
    is_alpha: default_is_alpha,
    is_printable: default_is_printable,
    lookup_name: default_lookup_name,
    locale_lower: c_locale_lower,
    locale_upper: c_locale_upper,
    locale_is_alnum: c_locale_is_alnum,
};

impl Default for UnicodeProvider {
    fn default() -> Self {
        DEFAULT_PROVIDER
    }
}

#[inline]
fn to_char(c: u32) -> Option<char> {
    char::from_u32(c)
}

pub fn default_is_word(c: u32) -> bool {
    if c < 128 {
        let b = c as u8;
        return b.is_ascii_alphanumeric() || b == b'_';
    }
    match to_char(c) {
        Some(ch) => ch.is_alphabetic() || ch.is_numeric(),
        None => false,
    }
}

/// First code point of every run of ten decimal digits (category Nd).
const ND_ZEROS: &[u32] = &[
    0x30, 0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66,
    0xDE6, 0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
    0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0,
    0xFF10, 0x104A0, 0x10D30, 0x10D40, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450,
    0x114D0, 0x11650, 0x116C0, 0x116D0, 0x116DA, 0x11730, 0x118E0, 0x11950, 0x11BF0, 0x11C50,
    0x11D50, 0x11DA0, 0x11F50, 0x16130, 0x16A60, 0x16AC0, 0x16B50, 0x16D70, 0x1CCF0, 0x1D7CE,
    0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E5F1, 0x1E950, 0x1FBF0,
];

pub fn default_is_digit(c: u32) -> bool {
    if c < 128 {
        return (0x30..=0x39).contains(&c);
    }
    if c < 0x660 {
        return false;
    }
    // ND_ZEROS is sorted; find the last start <= c.
    let idx = ND_ZEROS.partition_point(|&z| z <= c);
    if idx == 0 {
        return false;
    }
    let z = ND_ZEROS[idx - 1];
    c - z < 10 && to_char(c).is_some_and(|ch| ch.is_numeric())
}

pub fn default_is_space(c: u32) -> bool {
    matches!(
        c,
        0x09..=0x0D
            | 0x1C..=0x20
            | 0x85
            | 0xA0
            | 0x1680
            | 0x2000..=0x200A
            | 0x2028
            | 0x2029
            | 0x202F
            | 0x205F
            | 0x3000
    )
}

pub fn default_lower(c: u32) -> u32 {
    if c < 128 {
        return (c as u8).to_ascii_lowercase() as u32;
    }
    match to_char(c) {
        Some(ch) => ch.to_lowercase().next().map_or(c, |x| x as u32),
        None => c,
    }
}

pub fn default_upper(c: u32) -> u32 {
    if c < 128 {
        return (c as u8).to_ascii_uppercase() as u32;
    }
    match to_char(c) {
        Some(ch) => ch.to_uppercase().next().map_or(c, |x| x as u32),
        None => c,
    }
}

struct CaseTables {
    /// Sorted list of characters that belong to a class with >= 2 members.
    cased: Vec<u32>,
    /// Class index for each entry of `cased`.
    class_of: Vec<u32>,
    /// Each class: sorted member list.
    classes: Vec<&'static [u32]>,
}

fn case_tables() -> &'static CaseTables {
    static TABLES: OnceLock<CaseTables> = OnceLock::new();
    TABLES.get_or_init(build_case_tables)
}

fn build_case_tables() -> CaseTables {
    // lower(c) for every char whose mapping differs, grouped by target.
    let mut inverse: HashMap<u32, Vec<u32>> = HashMap::new();
    // full uppercase string -> distinct full lowercase characters
    let mut by_upper: HashMap<Vec<char>, Vec<u32>> = HashMap::new();
    // No character outside planes 0-1 has a case mapping; the big CJK,
    // Hangul and private-use blocks are caseless too.
    for cp in 0u32..0x20000 {
        if matches!(cp, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7A3 | 0xD800..=0xF8FF) {
            continue;
        }
        let Some(ch) = char::from_u32(cp) else { continue };
        let mut lo = ch.to_lowercase();
        let l0 = lo.next().map_or(cp, |x| x as u32);
        let lo_single = lo.next().is_none();
        let up: Vec<char> = ch.to_uppercase().collect();
        let up_is_self = up.len() == 1 && up[0] == ch;
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
    // Union-find over lowercase characters sharing an uppercase.
    let mut parent: HashMap<u32, u32> = HashMap::new();
    fn find(parent: &mut HashMap<u32, u32>, x: u32) -> u32 {
        let mut r = x;
        while let Some(&p) = parent.get(&r) {
            if p == r {
                break;
            }
            r = p;
        }
        // path compression
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
    // Build classes: key = representative lowercase char.
    let mut members: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut all_lowers: Vec<u32> = inverse.keys().copied().collect();
    for &k in parent.keys() {
        all_lowers.push(k);
    }
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
    CaseTables {
        cased: pairs.iter().map(|p| p.0).collect(),
        class_of: pairs.iter().map(|p| p.1).collect(),
        classes,
    }
}

pub fn default_case_class(c: u32) -> &'static [u32] {
    let t = case_tables();
    match t.cased.binary_search(&c) {
        Ok(i) => t.classes[t.class_of[i] as usize],
        Err(_) => &[],
    }
}

pub fn default_cased_chars() -> &'static [u32] {
    &case_tables().cased
}

pub fn default_id_start(c: u32) -> bool {
    if c < 128 {
        let b = c as u8;
        return b.is_ascii_alphabetic() || b == b'_';
    }
    match to_char(c) {
        // Approximation of XID_Start.
        Some(ch) => ch.is_alphabetic() && !is_combining_like(c),
        None => false,
    }
}

pub fn default_id_continue(c: u32) -> bool {
    if c < 128 {
        let b = c as u8;
        return b.is_ascii_alphanumeric() || b == b'_';
    }
    match to_char(c) {
        Some(ch) => {
            ch.is_alphanumeric() && !is_other_number(c) || is_combining_like(c) || c == 0xB7
        }
        None => false,
    }
}

fn is_combining_like(c: u32) -> bool {
    matches!(c, 0x300..=0x36F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// Characters in categories No/Nl that are not identifier characters
/// (a small approximation: super/subscripts, circled numbers, fractions).
fn is_other_number(c: u32) -> bool {
    matches!(
        c,
        0xB2 | 0xB3 | 0xB9 | 0xBC..=0xBE | 0x2070..=0x2079 | 0x2080..=0x2089 | 0x2150..=0x215F
            | 0x2460..=0x24FF | 0x2776..=0x2793 | 0x3220..=0x3229 | 0x3248..=0x324F
            | 0x3251..=0x325F | 0x3280..=0x3289 | 0x32B1..=0x32BF
    )
}

pub fn default_is_alpha(c: u32) -> bool {
    if c < 128 {
        return (c as u8).is_ascii_alphabetic();
    }
    to_char(c).is_some_and(|ch| ch.is_alphabetic())
}

pub fn default_is_printable(c: u32) -> bool {
    if c == 0x20 {
        return true;
    }
    if c < 0x20 || (0x7F..0xA0).contains(&c) || c == 0xAD {
        return false;
    }
    match to_char(c) {
        None => false,
        Some(ch) => {
            !(ch.is_whitespace()
                || ch.is_control()
                || matches!(c, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0xFEFF)
                || (0xE000..=0xF8FF).contains(&c)
                || c >= 0xF0000)
        }
    }
}

/// Built-in `\N{...}` table: only a handful of names (case-insensitive).  The
/// runtime is expected to provide a real `unicodedata.lookup`.
pub fn default_lookup_name(name: &str) -> Option<u32> {
    let up = name.to_ascii_uppercase();
    let table: &[(&str, u32)] = &[
        ("SPACE", 0x20),
        ("LESS-THAN SIGN", 0x3C),
        ("EQUALS SIGN", 0x3D),
        ("GREATER-THAN SIGN", 0x3E),
        ("LATIN CAPITAL LETTER A", 0x41),
        ("LATIN SMALL LETTER A", 0x61),
        ("EM DASH", 0x2014),
        ("SNAKE", 0x1F40D),
        ("LATIN CAPITAL LETTER A WITH DIAERESIS", 0xC4),
        (
            "ARABIC LIGATURE UIGHUR KIRGHIZ YEH WITH HAMZA ABOVE WITH ALEF MAKSURA ISOLATED FORM",
            0xFBF9,
        ),
    ];
    table.iter().find(|(n, _)| *n == up).map(|&(_, c)| c)
}

pub fn c_locale_lower(b: u8) -> u8 {
    b.to_ascii_lowercase()
}

pub fn c_locale_upper(b: u8) -> u8 {
    b.to_ascii_uppercase()
}

pub fn c_locale_is_alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

// ---------------------------------------------------------------------------
// Helpers corresponding to `_sre.ascii_tolower` & co.

pub fn ascii_tolower(c: u32) -> u32 {
    if c < 128 { (c as u8).to_ascii_lowercase() as u32 } else { c }
}

pub fn ascii_toupper(c: u32) -> u32 {
    if c < 128 { (c as u8).to_ascii_uppercase() as u32 } else { c }
}

pub fn ascii_iscased(c: u32) -> bool {
    c < 128 && (c as u8).is_ascii_alphabetic()
}

pub fn unicode_tolower(p: &UnicodeProvider, c: u32) -> u32 {
    (p.lower)(c)
}

pub fn unicode_iscased(p: &UnicodeProvider, c: u32) -> bool {
    c != (p.lower)(c) || c != (p.upper)(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        let k = default_case_class('k' as u32);
        assert_eq!(k, &[0x4B, 0x6B, 0x212A]);
        let s = default_case_class('s' as u32);
        assert_eq!(s, &[0x53, 0x73, 0x17F]);
        let i = default_case_class('i' as u32);
        assert_eq!(i, &[0x49, 0x69, 0x130, 0x131]);
        assert_eq!(default_case_class(0xFB05), &[0xFB05, 0xFB06]);
        assert!(default_case_class(0x1C80).contains(&0x412));
        assert!(default_case_class('1' as u32).is_empty());
        assert_eq!(default_lower(0x130), 'i' as u32);
    }

    #[test]
    fn skipped_blocks_are_caseless() {
        for cp in (0x3400u32..=0x4DBF).chain(0x4E00..=0x9FFF).chain(0xAC00..=0xD7A3).chain(0xE000..=0xF8FF).chain(0x20000..=0x10FFFF) {
            if let Some(ch) = char::from_u32(cp) {
                let l: Vec<char> = ch.to_lowercase().collect();
                let u: Vec<char> = ch.to_uppercase().collect();
                assert!(l == [ch] && u == [ch], "{cp:x}");
            }
        }
    }

    #[test]
    fn digits() {
        assert!(default_is_digit(0x37));
        assert!(default_is_digit(0xE58));
        assert!(default_is_digit(0xFF10));
        for c in [0x2165, 0x3039, 0x2082, 0x32B4] {
            assert!(!default_is_digit(c));
        }
    }
}
