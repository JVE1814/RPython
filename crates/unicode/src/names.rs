//! Character names: `unicodedata.name` / `unicodedata.lookup` and `\N{...}`.

use crate::tables::names::*;
use crate::{N_COUNT, S_BASE, S_COUNT, T_COUNT, V_COUNT};

/// Longest name (in bytes) any lookup can succeed for; longer inputs can be
/// rejected immediately.
pub const NAME_MAX_LEN: usize = if crate::tables::names::NAME_MAX_LEN > 128 {
    crate::tables::names::NAME_MAX_LEN
} else {
    128
};

static JAMO_L: [&str; 19] = [
    "G", "GG", "N", "D", "DD", "R", "M", "B", "BB", "S", "SS", "", "J", "JJ", "C", "K", "T", "P", "H",
];
static JAMO_V: [&str; 21] = [
    "A", "AE", "YA", "YAE", "EO", "E", "YEO", "YE", "O", "WA", "WAE", "OE", "YO", "U", "WEO", "WE",
    "WI", "YU", "EU", "YI", "I",
];
static JAMO_T: [&str; 28] = [
    "", "G", "GG", "GS", "N", "NJ", "NH", "D", "L", "LG", "LM", "LB", "LS", "LT", "LP", "LH", "M",
    "B", "BS", "S", "SS", "NG", "J", "C", "K", "T", "P", "H",
];

const HANGUL_PREFIX: &str = "HANGUL SYLLABLE ";

/// Result of [`Lookup`]-style name resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    Char(u32),
    /// A named sequence (only returned by `unicodedata.lookup`).
    Sequence(&'static [u32]),
}

// ---------------------------------------------------------------------------
// phrasebook decoding
// ---------------------------------------------------------------------------

fn word(k: usize) -> &'static [u8] {
    let mut p = WORD_BLOCKS[k / 16] as usize;
    for _ in 0..k % 16 {
        p += 1 + WORD_DATA[p] as usize;
    }
    let n = WORD_DATA[p] as usize;
    &WORD_DATA[p + 1..p + 1 + n]
}

/// Offset of explicit-name entry `i` in NAME_DATA.
fn entry(i: usize) -> usize {
    let mut p = NAME_BLOCKS[i / 16] as usize;
    for _ in 0..i % 16 {
        p += 4 + NAME_DATA[p] as usize;
    }
    p
}

fn entry_cp(p: usize) -> u32 {
    (NAME_DATA[p + 1] as u32) << 16 | (NAME_DATA[p + 2] as u32) << 8 | NAME_DATA[p + 3] as u32
}

/// Calls `f` with each byte chunk (words and separating spaces) of entry at `p`.
fn for_each_chunk(p: usize, mut f: impl FnMut(&[u8]) -> bool) {
    let n = NAME_DATA[p] as usize;
    let codes = &NAME_DATA[p + 4..p + 4 + n];
    let mut i = 0;
    let mut first = true;
    while i < codes.len() {
        let b = codes[i];
        let k = if b < WORD_ONE_BYTE {
            i += 1;
            b as usize
        } else {
            let k = WORD_ONE_BYTE as usize + (((b - WORD_ONE_BYTE) as usize) << 8 | codes[i + 1] as usize);
            i += 2;
            k
        };
        if !first && !f(b" ") {
            return;
        }
        first = false;
        if !f(word(k)) {
            return;
        }
    }
}

fn entry_name(p: usize, out: &mut String) {
    for_each_chunk(p, |w| {
        // names are pure ASCII
        out.push_str(std::str::from_utf8(w).unwrap());
        true
    });
}

/// Compares entry name at `p` with `key` (already upper-cased ASCII).
fn entry_cmp(p: usize, key: &[u8]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut pos = 0;
    let mut ord = Ordering::Equal;
    for_each_chunk(p, |w| {
        let rest = &key[pos..];
        let m = w.len().min(rest.len());
        match w[..m].cmp(&rest[..m]) {
            Ordering::Equal => {
                if w.len() > rest.len() {
                    ord = Ordering::Greater;
                    return false;
                }
                pos += w.len();
                true
            }
            o => {
                ord = o;
                false
            }
        }
    });
    if ord == Ordering::Equal && pos < key.len() {
        Ordering::Less
    } else {
        ord
    }
}

// ---------------------------------------------------------------------------
// name()
// ---------------------------------------------------------------------------

/// Appends the name of `c` to `out`; returns false (appending nothing) if the
/// character has no name.  Name aliases are never returned.
pub(crate) fn name_into(c: u32, out: &mut String) -> bool {
    if (S_BASE..S_BASE + S_COUNT).contains(&c) {
        let s = c - S_BASE;
        out.push_str(HANGUL_PREFIX);
        out.push_str(JAMO_L[(s / N_COUNT) as usize]);
        out.push_str(JAMO_V[((s % N_COUNT) / T_COUNT) as usize]);
        out.push_str(JAMO_T[(s % T_COUNT) as usize]);
        return true;
    }
    let i = name_index(c);
    if i != 0xFFFF {
        entry_name(entry(i as usize), out);
        return true;
    }
    for &(a, b, p) in ALGO_RANGES.iter() {
        if a <= c && c <= b {
            use std::fmt::Write;
            out.push_str(ALGO_PREFIXES[p as usize]);
            let _ = write!(out, "{c:04X}");
            return true;
        }
    }
    false
}

/// `unicodedata.name(c)` for the current database; `None` if unnamed.
pub fn name(c: u32) -> Option<String> {
    let mut s = String::new();
    if name_into(c, &mut s) {
        Some(s)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// lookup()
// ---------------------------------------------------------------------------

fn find_jamo(s: &[u8], table: &[&str]) -> Option<(usize, usize)> {
    // longest match
    let mut best: Option<(usize, usize)> = None;
    for (i, j) in table.iter().enumerate() {
        let j = j.as_bytes();
        if s.starts_with(j) && best.map_or(true, |(_, l)| j.len() > l) {
            best = Some((i, j.len()));
        }
    }
    best
}

fn lookup_hangul(rest: &[u8]) -> Option<u32> {
    let (l, n1) = find_jamo(rest, &JAMO_L)?;
    let rest = &rest[n1..];
    let (v, n2) = find_jamo(rest, &JAMO_V)?;
    let rest = &rest[n2..];
    let t = JAMO_T.iter().position(|j| j.as_bytes() == rest)?;
    Some(S_BASE + (l as u32 * V_COUNT + v as u32) * T_COUNT + t as u32)
}

fn lookup_algorithmic(key: &[u8]) -> Option<u32> {
    for (pi, prefix) in ALGO_PREFIXES.iter().enumerate() {
        let Some(hexpart) = key.strip_prefix(prefix.as_bytes()) else { continue };
        if hexpart.is_empty() || hexpart.len() > 6 || !hexpart.iter().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let v = u32::from_str_radix(std::str::from_utf8(hexpart).ok()?, 16).ok()?;
        // must be the canonical %04X spelling
        let canon_len = format!("{v:04X}").len();
        if canon_len != hexpart.len() {
            continue;
        }
        if ALGO_RANGES.iter().any(|&(a, b, p)| p as usize == pi && a <= v && v <= b) {
            return Some(v);
        }
    }
    None
}

/// Upper-cases an ASCII name into `buf`; None if not ASCII or too long.
fn normalize_key<'a>(name: &str, buf: &'a mut [u8; NAME_MAX_LEN]) -> Option<&'a [u8]> {
    let b = name.as_bytes();
    if b.len() > NAME_MAX_LEN || !b.is_ascii() {
        return None;
    }
    for (i, &c) in b.iter().enumerate() {
        buf[i] = c.to_ascii_uppercase();
    }
    Some(&buf[..b.len()])
}

/// Character (not alias, not sequence) lookup on an upper-cased key.
pub(crate) fn lookup_char_key(key: &[u8]) -> Option<u32> {
    if let Some(rest) = key.strip_prefix(HANGUL_PREFIX.as_bytes()) {
        if let Some(c) = lookup_hangul(rest) {
            return Some(c);
        }
    }
    if let Some(c) = lookup_algorithmic(key) {
        return Some(c);
    }
    // binary search over the sorted explicit names
    let (mut lo, mut hi) = (0usize, NAME_COUNT);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let p = entry(mid);
        match entry_cmp(p, key) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Some(entry_cp(p)),
        }
    }
    None
}

fn lookup_alias_key(key: &[u8]) -> Option<u32> {
    ALIASES.binary_search_by(|(n, _)| n.as_bytes().cmp(key)).ok().map(|i| ALIASES[i].1)
}

fn lookup_seq_key(key: &[u8]) -> Option<&'static [u32]> {
    NAMED_SEQUENCES.binary_search_by(|(n, _)| n.as_bytes().cmp(key)).ok().map(|i| NAMED_SEQUENCES[i].1)
}

/// Resolves a character name as `\N{...}` does: case-insensitive, accepts
/// name aliases, but not named sequences.
pub fn lookup_name(name: &str) -> Option<u32> {
    let mut buf = [0u8; NAME_MAX_LEN];
    let key = normalize_key(name, &mut buf)?;
    lookup_char_key(key).or_else(|| lookup_alias_key(key))
}

/// `unicodedata.lookup` for the current database (aliases and named sequences
/// accepted).
pub(crate) fn lookup_full(name: &str) -> Option<Lookup> {
    let mut buf = [0u8; NAME_MAX_LEN];
    let key = normalize_key(name, &mut buf)?;
    if let Some(c) = lookup_char_key(key).or_else(|| lookup_alias_key(key)) {
        return Some(Lookup::Char(c));
    }
    lookup_seq_key(key).map(Lookup::Sequence)
}

/// Lookup restricted to plain character names (used for `ucd_3_2_0`).
pub(crate) fn lookup_plain(name: &str) -> Option<u32> {
    let mut buf = [0u8; NAME_MAX_LEN];
    let key = normalize_key(name, &mut buf)?;
    lookup_char_key(key)
}

