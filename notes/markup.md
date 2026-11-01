# Workstream "markup" — notes

Branch: `worktree-agent-a76d7c540a0506f43`.

## Decisions

* **pyexpat = Rust engine + Python front end.**  The XML 1.0 parser is
  `crates/runtime/src/modules/pyexpat/engine.rs` (from scratch, no Expat code),
  exposed as the builtin module `_expat_engine` (`new_engine(namespace_separator,
  encoding)`; Engine methods `feed(data, final)` returning a list of event tuples
  `(kind, line, col, byte, *payload)`, `set_encoding_table`, `encoding_failed`,
  `external(context, encoding)`, `position()`, `started()`, `configure(...)`,
  `force_encoding(name)`; `ERROR_NAMES`/`ERROR_MESSAGES`).  `Lib/pyexpat.py`
  implements `xmlparser` (handlers, buffer_text, ordered/specified attributes,
  intern, namespaces/triplets, ParseFile, GetInputContext,
  ExternalEntityParserCreate, SetParamEntityParsing, UseForeignDTD, reparse
  deferral, amplification limits, `errors`/`model` submodules, EXPAT_VERSION
  'expat_2.7.3').  Rationale: handler dispatch, attribute buffering and error
  objects are Python-level semantics; the tokenizer/DTD/entity machinery is where
  speed matters.  Registration: one line each in `crates/runtime/src/modules/mod.rs`.
* Engine model: decoded `Vec<char>` buffer with per-char UTF-8 widths for byte
  offsets; newline normalisation in the decoder; provisional ASCII decoding until
  the XML declaration is read; UTF-16 auto-detection (BOM, `00 xx`/`xx 00`);
  unknown single-byte encodings via a 256-entry table built by
  `str(bytes(range(256)), name, 'replace')`.  Reparse deferral follows Expat's
  rule: only after an attempt that consumed nothing, retry when the pending data
  has doubled.
* `html.entities` is generated (`tools/markup/gen_html_entities.sh`) from the
  WHATWG entity list knowledge; 252 HTML 4 names + 2231 HTML 5 names.
* email: own implementation of `_header_value_parser`, policies, folding, etc.
  `email.utils`/`_parseaddr` avoid `calendar`/`urllib` (own `_timegm`, percent
  coding helpers).  Non-ASCII local parts are not a defect (RFC 6532).
* `_elementtree` (C accelerator) is **not** implemented; `xml.etree.ElementTree`
  is pure Python (test_xml_etree_c runs the Python implementation; the
  accelerator-only tests skip with "requires _elementtree").
* Testing overlay: `tools/markup/overlay.sh` builds /tmp/mklib from the
  integration checkout's Lib plus this worktree's packages (`EXTRA=<dir>` adds
  other worktrees' Lib, e.g. for http/urllib); `tools/markup/utv.sh <module>`
  runs a test module with the release binary.

## Test status (release build, overlay)

| test | result |
|---|---|
| test_pyexpat | 72/89 ok; 17 = CJK codecs missing (test_unsupported_encodings), test_exception (needs C-level traceback frame "pyexpat.c"), MalformedInputTest.test1 (fixed in engine, pending rebuild check) |
| test_xml_etree / _c | 228/238 ok; 16 CJK codecs; test_qname (runtime hash bug, below); XMLPullParser deferral + error position (fixed in engine, pending rebuild check) |
| test_minidom | 133/133 |
| test_pulldom | 11/11 |
| test_sax | 192/192 (needs urllib from the web workstream) |
| test_xml_dom_minicompat | 11/11 |
| test_xml_dom_xmlbuilder | blocked: test imports http.client + test_httplib (needs `array`) |
| test_email (package) | 1803/1814; rest = CJK codecs (8) + codec-name DeprecationWarning (2, runtime) + gb2312 |
| test_html, test_htmlparser | pass (see earlier runs) |
| test_optparse | 155/155 (translation snapshot test skipped: no Tools/i18n) |
| test_quopri, test_mimetypes, test_tomllib | pass where dependencies exist (tomllib needs datetime) |

## Bugs found outside this workstream (not fixed here)

* **`slot_tp_hash` reduces `__hash__` results modulo 2**61-1** even when they fit in
  Py_ssize_t (crates/runtime/src/types/slots.rs, `slot_tp_hash`).  CPython only
  re-hashes values outside the Py_ssize_t range.  Effect: `hash(obj)` with
  `__hash__ = lambda s: hash(s.text)` differs from `hash(s.text)`, so mixed
  str/wrapper dict keys miss (ElementTree QName, test_qname).  Fix: use the int
  value directly when it fits in i64 (-1 -> -2), else `int_hash`.
* `bytes.decode("utf-8”")` / codec fast path accepts non-ASCII codec names
  without going through `encodings.normalize_encoding`, so the 3.15
  DeprecationWarning ("Support for non-ascii encoding names ...") is not emitted.
* CJK codecs (euc-jp, shift_jis, iso-2022-jp, gb2312, big5, cp949, ...) missing.

## Missing

* `_elementtree` Rust accelerator.
* mailbox, smtplib, imaplib, poplib, ftplib, xmlrpc (see final report for the
  current state).
