# rpy-unicode — Unicode character database

Static tables (no start-up cost, no dependencies) for `str` predicates and case
mapping, the `unicodedata` module (current version and `ucd_3_2_0`), and
`\N{...}` name lookup.

## Unicode version: 17.0.0

Evidence from `spec/Lib/test/test_unicodedata.py` (CPython 3.15.2):

* explicit `# New in 17.0.0` expectations: `bidirectional('࢏') == 'AL'`,
  `bidirectional('\U0001fbfa') == 'ON'`, `decomposition('꟱') == '<super> 0053'`,
  `combining('᫏') == 230`, `east_asian_width('\U00016FF2') == 'W'`,
  `grapheme_cluster_break('᫫') == 'Extend'`, `block('\U00011B67') == 'Sharada Supplement'`,
  `indic_conjunct_break('္') == 'Linker'`, `extended_pictographic('⎈') is False`;
* names only present in 17.0: `CJK UNIFIED IDEOGRAPH-33479` (Ext. J),
  `TANGUT IDEOGRAPH-18D1E`, `test_ucn` `TANGUT IDEOGRAPH-18D1E`.
* Decisive: the SHA-1 checksums in the test file are reproduced exactly
  (see Tests) with the 17.0.0 data.

## Data and generator

* `data/17.0.0/`: UnicodeData, CompositionExclusions, EastAsianWidth,
  DerivedCoreProperties, DerivedNormalizationProps, SpecialCasing, CaseFolding,
  NameAliases, NamedSequences, PropList, Blocks, GraphemeBreakProperty,
  GraphemeBreakTest, emoji-data, DerivedBidiClass, DerivedName,
  NormalizationTest, Unihan_NumericValues (extracted from Unihan.zip).
* `data/3.2.0/`: UnicodeData, EastAsianWidth, CompositionExclusions,
  DerivedNormalizationProps, SpecialCasing, CaseFolding,
  NormalizationCorrections, and the numeric lines of Unihan-3.2.0.txt.
  (`NormalizationTest-3.2.0.txt` is in `spec/Lib/test/`.)
* Generator: `gen/` (standalone crate, own `[workspace]`). Regenerate with
  ```
  CARGO_TARGET_DIR=target/unicode cargo run --release --manifest-path crates/unicode/gen/Cargo.toml
  ```
  It writes `src/tables/{db,names,v320}.rs` (committed; normal builds need no data).
* Table data size: ~185 KB (properties/case/decomposition), ~561 KB (names:
  word phrasebook 328 KB + lexicon 101 KB + cp->name trie 80 KB + aliases /
  named sequences), ~8 KB (3.2.0 deltas). Total ≈ 0.75 MB.
* Tables: 3-level tries (shifts chosen by the generator to minimise size)
  mapping code point -> deduplicated record index (739 type records, 394 db
  records), a decomposition trie, a sorted composition-pair table.

## API (all code points are `u32`; > 0x10FFFF behaves like U+10FFFF)

Crate root — `str` support (CPython semantics):

```rust
pub const UNIDATA_VERSION: &str;            // "17.0.0"
pub fn is_alpha(c) -> bool;                 // gc in Lu Ll Lt Lm Lo
pub fn is_decimal(c) / is_digit(c) / is_numeric(c) / is_alnum(c) -> bool;
pub fn is_space(c) -> bool;                 // bidi WS/B/S or gc Zs
pub fn is_printable(c) -> bool;             // gc not C*/Z*, or U+0020
pub fn is_lowercase(c) / is_uppercase(c)    // Lowercase / Uppercase properties
pub fn is_titlecase(c)                      // gc Lt
pub fn is_cased(c) / is_case_ignorable(c) / is_xid_start(c) / is_xid_continue(c) -> bool;
pub fn is_linebreak(c) -> bool;             // str.splitlines set
pub fn is_identifier(s: &[u32]) -> bool;    // str.isidentifier (no NFKC step)
pub fn to_decimal(c) / to_digit(c) -> Option<u32>;
pub fn to_numeric(c) -> Option<f64>;        // incl. fractions and Unihan values
pub fn to_lower_simple(c) / to_upper_simple(c) / to_title_simple(c) -> u32;  // UnicodeData
pub fn py_to_lowercase(c) / py_to_uppercase(c) / py_to_titlecase(c) -> u32;  // CPython _PyUnicode_To*case
pub fn py_to_numeric(c) -> f64;             // -1.0 if none
pub fn to_lower_full(c, &mut [u32;3]) -> usize;   // SpecialCasing unconditional
pub fn to_upper_full / to_title_full / casefold_full (same signature)
pub fn has_special_casing(c) -> bool;
pub fn is_final_sigma(before_rev: impl IntoIterator<Item=u32>, after: impl IntoIterator<Item=u32>) -> bool;
pub fn lower_full_at(s: &[u32], i: usize, out: &mut [u32;3]) -> usize; // lower() incl. final sigma
```

Python-level string methods built on these (see `tests/common/mod.rs` for a
reference emulation used by the checksum test): `lower` = `lower_full_at` per
index; `upper` = `to_upper_full`; `title` = `to_title_full` if previous char
not cased else `lower_full_at`, `previous_is_cased = is_cased(c)`;
`capitalize` = title-full first + `lower_full_at` rest; `swapcase` =
`is_uppercase -> lower_full_at`, `is_lowercase -> to_upper_full`;
`casefold` = `casefold_full`. `islower/isupper/istitle` follow CPython's
loops with the single-character shortcut (`istitle(c)` = Lt or Uppercase).

`unicodedata` (current version), free functions:

```rust
pub fn category(c) -> &'static str; bidirectional(c) -> &'static str;
pub fn combining(c) -> u8; east_asian_width(c) -> &'static str; mirrored(c) -> bool;
pub fn decomposition(c) -> String;          // "<compat> 0020 0301", Hangul algorithmic
pub fn decomposition_mapping(c) -> Option<(&'static str, DecompSeq)>;
pub fn name(c) -> Option<String>;           // never returns aliases
pub fn lookup_name(&str) -> Option<u32>;    // \N{...}: case-insensitive, aliases yes, sequences no
pub fn normalize(Form, &[u32]) -> Vec<u32>; is_normalized(Form, &[u32]) -> bool;
pub fn normalize_if_needed(Form, &[u32]) -> Option<Vec<u32>>; // None = already normalized
pub enum Form { NFC, NFD, NFKC, NFKD }  // Form::from_name("NFC")
pub fn block(c) -> &'static str;            // "No_Block" default      (3.15 new)
pub fn grapheme_cluster_break(c) -> &'static str;                     (3.15 new)
pub fn indic_conjunct_break(c) -> &'static str;                       (3.15 new)
pub fn extended_pictographic(c) -> bool;                              (3.15 new)
pub fn next_grapheme_break(s: &[u32], start, end) -> usize;           // iter_graphemes core
pub fn next_grapheme_break_by(get: impl Fn(usize)->u32, start, end) -> usize;
pub struct Graphemes<'a>; Graphemes::new(s, start, end): Iterator<Item=(usize, usize)>
```

`Ucd` — the `unicodedata.UCD` objects (`Ucd::Current` = module,
`Ucd::V3_2_0` = `unicodedata.ucd_3_2_0`), methods taking `self`:
`unidata_version, category, bidirectional, combining, east_asian_width,
mirrored, decimal, digit, numeric, decomposition, name, name_into, lookup ->
Option<Lookup> (Lookup::Char(u32) | Lookup::Sequence(&'static [u32])),
normalize, normalize_if_needed, is_normalized, isxidstart, isxidcontinue`.
`None` results map to ValueError/KeyError or the default argument.

### 3.2.0 semantics (reverse-engineered from the tests, confirmed by checksum)

For characters not assigned in 3.2.0: category "Cn", bidirectional "",
combining 0, mirrored 0, decimal/numeric none, decomposition "", no name,
east_asian_width "N" if assigned now else the current default. For assigned
characters the 3.2.0 values of category, bidirectional, east_asian_width,
mirrored, decimal and numeric (incl. Unihan 3.2.0) are used; digit, combining,
decomposition string and names come from the current data. lookup() accepts
neither aliases nor named sequences and only finds characters assigned in
3.2.0. Normalization skips decomposing unassigned characters, applies the
3.2.0 decompositions of the 6 corrected CJK compatibility ideographs, and
composes with the current tables (no quick check).

## Tests (`cargo test -p rpy-unicode`)

* `tests/checksums.rs`: SHA-1 (own implementation) reproduction of
  `test_method_checksum` (quick + full) and `test_function_checksum` for the
  current and the 3.2.0 database (quick + full) — all 6 match.
* `tests/datafiles.rs`: NormalizationTest.txt 17.0.0 and 3.2.0 (incl. the
  "all other characters are invariant" part), normalize consistency over all
  code points for both databases, DerivedName.txt for every code point, name
  inverse lookup (both databases, also lower-cased), all named sequences and
  aliases, GraphemeBreakTest.txt (also starting at every break).
* `tests/spec.rs`: the explicit expectations of test_unicodedata.py,
  test_ucn.py, test_str.py (case mapping, isidentifier, isprintable,
  surrogates), test_unicode_identifiers.py.
* `tests/perf.rs` (ignored): ~0.8 ns per predicate, ~2 ns per full case
  mapping, ~0.7 µs per lookup_name, ~0.2 µs per name().

## Gaps / notes

* `py_to_lowercase` etc. reproduce CPython's `_PyUnicode_To*case` from
  memory of its documented behaviour (first code point of the full mapping);
  not covered by a spec test.
* `iter_graphemes(s, start, end)` slice clamping and the `Segment` object are
  the runtime's job; this crate segments `s[start..end]` as standalone text.
* Name lookup returns `None` for over-long names (CPython: KeyError).
