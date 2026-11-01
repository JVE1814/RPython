# rpy-sre — regular expression engine (CPython `re` semantics)

Crate `rpy-sre` (`crates/sre`), no dependencies. Written from scratch from the
behaviour pinned by `spec/Lib/test/test_re.py` and `re_tests.py`.

## Layout

| file | role |
|------|------|
| `src/parser.rs` | tokenizer + parser → AST, every `re.PatternError` message/position, FutureWarnings, group widths |
| `src/compiler.rs` | AST → instruction program; char sets; case folding closure; search analysis (literal prefix, first-char set, 2-char pair filter, anchoring) |
| `src/vm.rs` | backtracking VM with explicit stack (`Scratch`), generic over code unit `u8`/`u16`/`u32` |
| `src/search.rs` | match / fullmatch / search drivers, prefilters, memchr |
| `src/template.rs` | `re.sub` replacement template parsing and expansion |
| `src/unicode.rs` | `UnicodeProvider` (function-pointer table) + default implementation |
| `src/error.rs` | `CompileError`, `TemplateError`, `Warning`, line/column helpers |

## Public API

```rust
// flags (same values as re.*):
rpy_sre::flags::{TEMPLATE, IGNORECASE, LOCALE, MULTILINE, DOTALL, UNICODE, VERBOSE, DEBUG, ASCII}
pub const MAXREPEAT: u32 = u32::MAX;      // _sre.MAXREPEAT (= "infinite")
pub const MAXGROUPS: usize = 0x3FFF_FFFF; // _sre.MAXGROUPS
pub const CODESIZE: usize = 4;
pub const MAGIC: u32;

pub enum PatternSource<'a> { Latin1(&[u8]), Ucs2(&[u16]), Ucs4(&[u32]), Bytes(&[u8]) }
pub fn compile(src: PatternSource, flags: u32) -> Result<Pattern, CompileError>;
pub fn compile_with(src: PatternSource, flags: u32, prov: &UnicodeProvider) -> Result<Pattern, CompileError>;
pub fn compile_str(&str, flags) / compile_bytes(&[u8], flags)          // conveniences

pub enum CompileError {
    Pattern { msg: String, pos: Option<usize> }, // re.PatternError (msg/pos exactly as CPython)
    Overflow(String),                           // OverflowError ("the repetition number is too large")
    Value(String),                              // ValueError (flag incompatibilities, fix_flags)
}
pub fn line_col(pattern: &[u32], pos) -> (lineno, colno);
pub fn format_error_message(pattern, msg, pos) -> String;   // "msg at position N (line L, column C)"

impl Pattern {
    fn pattern(&self) -> &[u32];              // source code points / byte values
    fn is_bytes(&self) -> bool;
    fn flags(&self) -> u32;                   // Pattern.flags (inline global flags + implicit UNICODE)
    fn groups(&self) -> usize;                // Pattern.groups
    fn group_index(&self) -> &[(String, usize)]; // Pattern.groupindex, definition order
    fn group_by_name(&self, &str) -> Option<usize>;
    fn group_name(&self, usize) -> Option<&str>; // for Match.lastgroup
    fn warnings(&self) -> &[Warning];         // FutureWarning "Possible nested set at position N", ...
    fn exec<T: CodeUnit>(&self, s: &[T], pos, endpos, mode: MatchMode) -> Option<Captures>;
    fn exec_with<T>(&self, &mut Scratch, s, pos, endpos, mode, must_advance: bool) -> Option<Captures>;
    fn match_at / fullmatch / search(&self, s: &[T], pos, endpos) -> Option<Captures>;
    fn scanner(&self, s: &[T], pos, endpos) -> Scanner;   // finditer/findall/sub/split driver
    fn parse_template(&self, repl: &[u32]) -> Result<Vec<TemplateItem>, TemplateError>;
}
pub enum MatchMode { Prefix /* match, prefixmatch */, Full, Search }
pub struct Captures { pub spans: Vec<(isize, isize)> /* [0] = whole match, (-1,-1) unmatched */,
                      pub lastindex: Option<usize> }
impl Scanner { fn search(&mut self) -> Option<Captures>; fn match_(&mut self) -> Option<Captures>; fn pos(); }
impl Iterator for Scanner (= repeated search())
pub trait CodeUnit: u8 | u16 | u32      // PEP 393 kinds / bytes; positions are code-unit indices
pub struct Scratch;                     // reusable matcher memory (exec uses a thread-local pool)

pub enum TemplateItem { Literal(Vec<u32>), Group(usize) }
pub enum TemplateError { Pattern { msg, pos }, Index(String) /* IndexError: unknown group name 'x' */ }
pub fn expand_template(items, group: impl FnMut(usize) -> Option<&[u32]>, out: &mut Vec<u32>);

// _sre helpers
pub fn ascii_iscased(u32) -> bool; ascii_tolower(u32) -> u32;
pub fn unicode_iscased(&UnicodeProvider, u32) -> bool; unicode_tolower(&UnicodeProvider, u32) -> u32;
```

### How the runtime should use it

* `re.compile`: build `PatternSource` straight from the PEP 393 buffer (no
  copy of the subject is ever needed; the pattern itself is copied once).
  Map `CompileError` to `re.PatternError(msg, pattern, pos)` /
  `OverflowError` / `ValueError`.  Emit `pattern.warnings()` with
  `warnings.warn(msg, FutureWarning)`.
* `Pattern.match/fullmatch/search`: call `exec` with the subject's storage
  slice (`&[u8]` for UCS1 and bytes-like, `&[u16]`, `&[u32]`).  `pos`/`endpos`
  are clamped exactly like CPython; `pos > endpos` yields no match.
  `^`/`\A` refer to the real start of the string, `$`/`\Z` to `endpos`,
  lookbehind and `\b` may look before `pos`.
* finditer / findall / sub / subn / split: create one `Scanner` and call
  `search()` repeatedly; it implements the 3.7+ empty-match rules (an empty
  match is rejected at the position where the previous match ended iff that
  previous match was empty).  `split` takes `string[last:m.start()]` pieces
  plus the groups, and the tail after the last match; `sub` concatenates the
  gaps with the expansion.  `Scanner::match_()` implements `scanner.match()`
  (used by `re.Scanner`).
* `Match.lastindex` = `Captures::lastindex`; `lastgroup` = `group_name(i)`.
* sub templates: `pattern.parse_template(repl)` → items, cache per repl
  string; expand with `expand_template` (unmatched group → empty).  Template
  errors: `TemplateError::Pattern` → `re.PatternError(msg, repl, pos)`,
  `TemplateError::Index` → `IndexError(msg)`.
* `re.escape`, `Pattern.__repr__`, flag repr, pickling, `re.Scanner` class,
  `_compiler._generate_overlap_table` etc. are runtime/Python-level.
* LOCALE: `UnicodeProvider::locale_*` are called at *match* time, so plug in
  libc `tolower`/`toupper`/`isalnum` to honour `setlocale` changes.

### UnicodeProvider

Plain struct of `fn` pointers (`is_word`, `is_digit`, `is_space`, `lower`,
`upper`, `case_class`, `cased_chars`, `is_identifier_start/continue`,
`is_alpha`, `is_printable`, `lookup_name`, `locale_lower/upper/is_alnum`).
`DEFAULT_PROVIDER` is exact for ASCII/Latin-1 and uses Rust `char` for the
rest (\w uses `char::is_alphanumeric`, which slightly differs from
`str.isalnum` for some combining marks; \d uses a built-in Nd table;
`lookup_name` only knows a handful of names).  The runtime should build a
provider from its `unicodedata` tables:

* `lower(c)`/`upper(c)`: *first* char of the full case mapping (=
  `_PyUnicode_ToLowercase`).
* `case_class(c)`: sorted list of all chars `y` with `lower(y) ∈ G(lower(c))`
  where `G(l)` = `{l}` ∪ other lowercase chars that share the same full
  uppercase (CPython `_casefix` rule).  Empty if the class is `{c}`.
* `cased_chars()`: sorted union of all non-trivial classes.

## Semantics notes / design decisions

* Captures use a *trail*: every slot / lastindex / repeat-counter write is
  undone on backtracking, which reproduces CPython's (post issue35859)
  results for all the mark tests.  Groups inside repeats keep the value of
  the last iteration that set them.
* General repeats follow SRE's MAX_UNTIL/MIN_UNTIL rules (an iteration that
  matched the empty string ends the loop but is kept).  Possessive repeats
  match each iteration atomically (minimum iterations included), like SRE's
  POSSESSIVE_REPEAT.  `X*`/`X+` with a body that cannot match empty compile to
  a plain loop; single-character bodies use `RepeatOne` fast loops.
* IGNORECASE (unicode) is compiled into membership tests of the case closure
  (no lowering at match time); backreferences compare `lower()` like
  GROUPREF_UNI_IGNORE.  ASCII mode only folds A-Z/a-z.  Bytes patterns use
  ASCII semantics unless LOCALE.
* `\B` matches in the empty string, `\b` does not (3.14+ behaviour).
* Errors without position (as in CPython's compiler): "look-behind requires
  fixed-width pattern", "looks too much behind" (width > 2**32-1).
* Regex DEBUG output (`re.DEBUG` dump) is not produced.

## Performance

* literal prefix → memchr (SWAR for `u8`) + verify, VM resumes after prefix;
  pure literal patterns never enter the VM;
* first-character bitmap and 2-character pair filter for search;
* `^`/`\A`-anchored patterns try only position 0;
* min-width bound on search start positions;
* `SplitG`: alternation skips an alternative without pushing a frame when
  its first character does not match;
* `RepeatOne` greedy/lazy with tail-literal hint (skip positions where the
  following literal cannot match);
* no allocation per attempt (thread-local `Scratch` pool; slots only reset
  when touched).

`cargo run --release --example bench -p rpy-sre` runs the micro benchmark.

## Tests

* `tests/re_tests.rs` reads `spec/Lib/test/re_tests.py` at test time and runs
  the whole `tests` table and `benchmarks` the way `test_re.ExternalTests`
  does (including bytes/LOCALE/range-limited/IGNORECASE/UNICODE variants).
* `tests/test_re.rs`: port of test_re.py (106 test functions; everything that
  concerns the engine, parser errors, templates, split/sub/findall rules).
* `tests/stress.rs`: 10^6-char inputs, code-unit widths, locale provider
  switching at match time, pos/endpos.
