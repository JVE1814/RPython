# Workstream "intl" — notes

CJK multibyte codecs, codec-name handling, source-encoding decoding, codec fixes.

## Layout (crates/runtime/src/modules/cjk)

| file | content |
|------|---------|
| `mod.rs` | `MbCodec` trait (encode/decode with an 8-byte state, `Fail::{Illegal(n), TooFew, Internal}`), `CodecDef`, the `_codecs_cn/jp/kr/tw/hk/iso2022` modules (`getcodec(name)`), registration. |
| `mbc.rs` | `_multibytecodec`: `MultibyteCodec` (not exported, like CPython), `MultibyteIncrementalEncoder/Decoder`, `MultibyteStreamReader/Writer`, `__create_codec` (stub); the conversion driver that applies error handlers. |
| `dbcs.rs` | gb2312, gbk, gb18030, hz, shift_jis, cp932, euc_jp, shift_jis_2004, shift_jisx0213, euc_jis_2004, euc_jisx0213, euc_kr, cp949, johab, big5, cp950, big5hkscs. |
| `iso2022.rs` | iso2022_jp, _1, _2, _2004, _3, _ext, iso2022_kr (designation tables, escape parsing, SO/SI, ESC N single shift). |
| `table.rs` | `Dbcs` (per-lead-byte trail ranges over a `u16` array; `0xFFFE` slots resolved in a sorted `wide` list holding supplementary code points and combining pairs) and `Rev` (lazily built reverse maps). |
| `data_{cn,jp,kr,tw,hk}.rs` | **generated** by `tools/gencjk` — do not edit. |

## Tables

`tools/gencjk/fetch.sh` downloads the reference mapping tables that test_codecmaps_* use
(pythontest.net mirrors of unicode.org / Microsoft / x0213.org / HKSCS-2004 tables) plus
glibc's EUC-JISX0213 charmap; `cargo run --release --manifest-path tools/gencjk/Cargo.toml
-- CACHE .` writes the Rust tables (`--report` prints the overlap analysis).  Charsets are
stored as base + overlay (GBK = GB2312 + GBKEXT, GB 18030 = GBK + GB18030EXT + four-byte
runs, CP932EXT over JIS X 0208, JIS X 0213 plane 1 = JIS X 0208 + JISX0213_1, CP949EXT over
KS X 1001, CP950EXT over Big5, HKSCS); the generator asserts the overlays reproduce the
reference tables.  Encoding maps are built on first use by walking the decode tables in
ascending code order ("first code wins", which is what test_codecmaps checks).

## Behaviour decisions (from the tests + reference tables)

* Error sizes: an undecodable lead/trail combination is `Illegal(1)` (one byte), so
  `replace` resyncs on the next byte; ISO-2022 two-byte sets report the set width; an
  unterminated escape at the end is "incomplete" (one U+FFFD for the rest).
* Error callbacks: strict/ignore/replace inline (replace encodes `?` through the codec, so
  ISO-2022 switches back to ASCII first); other handlers get one reused exception object;
  `(str|bytes, int)` results, str replacements are encoded strictly with the codec state,
  positions may move backwards/forwards, out-of-range/overflowing positions are IndexError.
* Incremental objects keep pending input (encoder: characters that may start a combining
  pair; decoder: incomplete sequences, max 8 bytes) and only commit state/pending on
  success.  Encoder state: `int.from_bytes([len(pending_utf8)] + pending_utf8 + state8,
  'little')`; decoder state: `(pending, int.from_bytes(state8, 'little'))`.
* ISO-2022 state bytes: G0..G3 designations (final byte, |0x80 for 94^2 sets), byte 4 =
  SO flag.  Encoder starts with G0=G1=ASCII, decoder also G2=ASCII.  JIS X 0208 /
  GB 2312 use `ESC $ F`, other 94^2 sets `ESC $ ( F` / `ESC $ ) F`.  iso2022_jp_2 never
  encodes through G2 (Latin-1/Greek are decode-only via `ESC . A/F` + `ESC N`).
  JIS X 0213 variants try a pairs-only plane-1 designation first, then JIS X 0208.
* JIS: the plain JIS X 0208 table keeps 0xA1C0 = U+005C; EUC-JP / Shift_JIS / EUC-JIS-2004 /
  ISO-2022 decode it as U+FF3C (and encode U+FF3C to it), Shift_JIS-2004 decodes 0x815F as
  U+005C and single bytes 0x5C/0x7E as U+00A5/U+203E.  cp932 decodes the user-defined area
  F040-F9FC as U+E000.. and 0x80/0xA0/0xFD-0xFF as U+0080/U+F8F0-U+F8F3; it also encodes the
  JIS X 0208 mappings CP932 replaced (U+2016, U+2212, ...).  The :2000 codecs reject the ten
  JIS X 0213:2004 additions and map 2-93-27 to U+9B1D (:2004: U+9B1C).
* euc_kr encodes/decodes the KS X 1001:1998 eight-byte make-up sequences (A4D4 + jamo);
  johab computes hangul arithmetically and maps symbols/hanja through KS X 1001.
* big5 decodes the reference table's undefined codes A15A/A1C3/A1C5/A1FE/A240/A2CC/A2CE like
  CP950 (decode only); big5hkscs ignores Big5 in C6A1-C8FE and prefers HKSCS codes when
  encoding; 8862/8864/88A3/88A5 are Ê/ê + U+0304/U+030C pairs.
* gb18030 is the 2000 edition (as gb-18030-2000.xml): four-byte BMP mapping by monotone
  runs, U+10000.. from 0x90308130.

## Other fixes in this workstream

* Encoding names: str.encode/bytes.decode only take the fast paths for ASCII names;
  names with lone surrogates go through `_codecs::lookup_obj`: DeprecationWarning, then
  LookupError (no UTF-8 form, so never a registered codec) — test_email rfc2231 and
  encoded-word cases (CharsetError defect).
* `ks_c_5601_1987` is an alias of cp949 (test_email requires `'갂'.encode('ks_c_5601-1987')`
  to give the UHC bytes).  email.charset maps hz -> hz-gb-2312, big5hkscs -> big5-hkscs;
  contentmanager sends euc-jp / shift_jis text as iso-2022-jp (CHARSETS conversion).
* Source decoding (compile.rs `decode_hook`): registry lookup first (unknown -> "unknown
  encoding: X"), then the text-encoding check ("'hex_codec' is not a text encoding; ...")
  and the codec; decoding SyntaxErrors no longer carry a "(unicode error)" prefix; the
  BOM/cookie conflict error carries the line text (parser tokenizer.rs, minimal edits).
* `\N{...}` lookups (compile, unicode_escape, namereplace) import `unicodedata`; if it cannot
  be imported compile raises "(unicode error) \N escapes not supported (can't load
  unicodedata module)".
* `UnicodeError.start/.end` attributes return the raw values (only the C API getters clamp);
  the Python-visible error handlers clamp like the C getters.
* UTF-8: surrogateescape writes escaped bytes and hands the rest of a surrogate run to the
  handler; an incremental decoder waits on a truncated `ED A0..BF` (surrogatepass).
* Final `*_decode(..., final=True)` report the original input length as consumed.

## Testing notes

test_codecmaps_* need `urllib` (open_urlresource) and skip without the `urlfetch` resource;
with the reference files placed in a copy of Lib/test/data they all pass (verified).

## Status (at pause)

Passing locally: test_multibytecodec, test_codecencodings_{cn,hk,iso2022,jp,kr,tw}, test_codecs,
test_charmapcodec, test_codeccallbacks, test_utf8source, test_source_encoding, test_py_compile
(utf-7 cookie), test_unicode_file*, all test_codecmaps_* (with the mapping files in a copy of
Lib/test/data and a stub urllib; real runs need the web workstream's urllib).
Committed but not yet verified on a fresh build: unicodedata GC support (test_reference_loops),
`ensure_unicodedata` (test_unicodedata_unload_reload / test_failed_import_during_compiling),
the email charset fixes (6 test_email failures).

## Next steps

* Verified on the final build (after merging master): test_email, test_unicodedata,
  test_multibytecodec, test_codecencodings_*, test_codeccallbacks, test_source_encoding pass.
* test_codecs: only LocaleCodecTest fails — it is newly un-skipped by master's
  `_testinternalcapi` (DecodeLocaleEx/EncodeLocaleEx semantics), not yet looked at.
* test_subinterp (test_multibytecodec) needs _testcapi.run_in_subinterp.
* `_multibytecodec.__create_codec` is a stub; no GC traversal for the stream reader/writer
  `stream` reference.
* test_xml_etree XInclude errors (urllib) and test_pyexpat traceback depth are not codec issues.
