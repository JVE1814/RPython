# Workstream "text" — notes

Regex, codecs, unicodedata, tokenizer and text utility modules.

## Rust modules (crates/runtime/src/modules)

| file | module | notes |
|------|--------|-------|
| `_sre.rs` | `_sre` | `re.Pattern` / `re.Match` / `_sre.SRE_Scanner` / `_sre.SRE_Template` over `rpy-sre`. |
| `_codecs.rs`, `codec_impl.rs`, `codec_charmap.rs` | `_codecs` | codec + error-handler registries, every built-in codec. |
| `codecs_core.rs` | (internal) | `str.encode` / `bytes.decode` entry points, now delegating to `_codecs`. |
| `unicodedata.rs` | `unicodedata` | module functions, `UCD` type, `ucd_3_2_0`, 3.15 `block`, `grapheme_cluster_break`, `indic_conjunct_break`, `extended_pictographic`, `iter_graphemes`/`Segment`. |
| `_tokenize.rs` | `_tokenize` | `TokenizerIter(readline, *, extra_tokens, encoding=None)` over `rpy_parser::tokenizer::TokenIter`. |
| `_locale.rs` | `_locale` | setlocale, localeconv, strcoll/strxfrm (wcscoll/wcsxfrm), nl_langinfo (glibc item values), getencoding, gettext bindings, `Error`. |
| `text_util.rs` | — | shared helpers (`StrBuf`, `substr`, `HeldBuffer` buffer exports, `register()` for all modules above). |
| `types/templatelib.rs` | — | `string.templatelib.Template` / `Interpolation` / `TemplateIter`, `new_template` / `new_interpolation` used by BUILD_TEMPLATE / BUILD_INTERPOLATION. |

`_string` is provided by the str workstream (`str_methods::init_string_module`); the
copy written here was dropped at the merge.

## Decisions

* **`_sre.compile` keeps CPython's signature** `(pattern, flags, code, groups, groupindex,
  indexgroup)` (test_re calls it directly) but only validates `code` (TypeError /
  OverflowError like CPython) and compiles `pattern` with the Rust parser/compiler.
  `re._compiler.compile(p, flags)` calls `_sre.compile(p, flags, [], 0, {}, ())`.
  `re._parser` / `re._compiler` / `re._constants` are compatibility shims (constants,
  `PatternError`, `_generate_overlap_table`).
* Pattern errors are raised as `re.PatternError(msg, pattern, pos)` (imported lazily from
  `re`); compile-time FutureWarnings use stacklevel 4 (user -> re.compile -> re._compile
  -> re._compiler.compile).
* Unicode provider for the regex engine is built from `rpy-unicode` (exact `str.isalnum`
  / `isdecimal` / `isspace`, `_PyUnicode_ToLowercase` semantics; case classes computed
  lazily with the `_casefix` rule; LOCALE uses libc tolower/toupper/isalnum at match time).
* Replacement templates are parsed by `rpy_sre::Pattern::parse_template` and cached per
  pattern (8 entries); `_sre.template()` only validates its arguments.
* Scanners over buffer objects (bytearray, memoryview) hold the buffer export until the
  scanner dies (bytearray resize raises BufferError, test_keep_buffer); `Match.group()` on
  such subjects re-reads the current buffer and clamps (test_bug_29444).
* `re.Scanner` builds `(p1)|(p2)|...` from the phrases (rejects phrases with groups, as
  3.15 does); inline global flags inside phrases are not supported.
* `codecs.lookup` normalization is the 3.15 one: ASCII lower-case, spaces -> hyphens,
  DeprecationWarning for non-ASCII names. The str.encode/bytes.decode fast paths use the
  C-level punctuation-collapsing normalization and only cover utf-8/16/32, ascii, latin-1
  (as CPython); everything else goes through the registry, which adds the
  "encoding with 'x' codec failed" note and the text-encoding checks.
* Error handlers: built-in names take Rust fast paths that produce exactly what the
  Python-visible handler functions produce; custom handlers get one reused exception
  object; decoders re-read `exc.object` after each handler call (mutating handlers).
* Single-byte charmap codecs are **generated**: `tools/gencodecs/fetch.sh` converts the
  unicode.org MAPPINGS tables (ISO 8859, Microsoft Windows/PC/EBCDIC, Apple, misc) and
  glibc charmaps (IBM273, CP1125, HP-ROMAN8, KOI8-T, PT154, IBM858, TIS-620) into
  `tools/gencodecs/data/*.map`; cp1140 = cp037 + euro, palmos = cp1252 + card suits
  (approximation), cp720 from the code page chart (`cp720.src`). `cargo run
  --manifest-path tools/gencodecs/Cargo.toml` writes `Lib/encodings/<name>.py`.
  `tools/gencodecs/gen_cjk.sh` writes the CJK wrapper modules.
* `unicodedata.lookup` raises `KeyError("undefined character name '...'")`, `name` raises
  `ValueError("no such name")`.
* `_tokenize.TokenizerIter` reads the whole source through `readline` on the first
  `next()` (the parser tokenizer needs the full text), so an endless readline never
  returns.
* `gettext.c2py` uses an own recursive-descent parser building closures (no `eval`),
  with C semantics (`!` binds tightest, truncating `/` and `%`, 0/1 comparisons, length
  limit 1000, parenthesis depth limit 20).
* `locale.locale_alias` is a smaller, self-consistent table (all names checked by
  test_locale's NormalizeTest are present); `_append_modifier` drops `@euro` for UTF-8 and
  switches ISO8859-1 to ISO8859-15.
* `pprint` 3.15 `expand=True` output format was inferred from test_pprint.
* `difflib.HtmlDiff` templates were taken from the golden file
  `spec/Lib/test/test_difflib_expect.html`.

## Missing / known gaps

* CJK codecs: `_codecs_cn/jp/kr/tw/hk/iso2022` and `_multibytecodec` are not written
  (the `encodings` wrapper modules exist, lookups fail with LookupError).
* Windows-only codecs (mbcs, oem, code_page_*) not provided.
* `re.DEBUG` dump (test_re DebugTests) is not produced.
* `glob._StringGlobber` / `_PathGlobber` (pathlib internals) are not implemented.
* No GC traversal for Pattern/Match/Segment objects (runtime GC not ready yet).
* Most spec test files could not be run yet: the runtime still lacks `enum`, `io`,
  many bytes/bytearray methods, big-int `range`, etc.  Verified by hand: `_sre`
  (match/search/sub/split/findall/finditer/Scanner/IGNORECASE/Unicode classes),
  all codecs incl. error handlers, charmap modules, unicodedata, t-strings,
  `_tokenize`, `_locale`, `stringprep`, textwrap.
