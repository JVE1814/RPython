# Workstream "markup2" — notes

Branch: `worktree-agent-ae303a4032911a34a` (merged master at the end).

## Done

* **Native `pyexpat`** (crates/runtime/src/modules/pyexpat/xmlparser.rs), replaces
  Lib/pyexpat.py.  `xmlparser` type (handlers as getset descriptors, buffer_text with
  a Rust string buffer, ordered/specified attributes, intern dict, namespaces,
  ExternalEntityParserCreate, SetParamEntityParsing, UseForeignDTD, reparse deferral,
  billion-laughs limits, **allocation tracker** `SetAllocTracker{ActivationThreshold,
  MaximumAmplification}` — modelled in engine.rs as buffered input + DTD storage + a
  1 KiB base), `errors`/`model` submodules, ExpatError.  A failing handler gets a
  synthetic traceback entry `pyexpat.c` / `<HandlerName minus "Handler">` (via
  PyCode_NewEmpty + PyFrame_New), so test_exception passes naturally.
  The old `_expat_engine` builtin is removed (nothing used it).
* **`_elementtree`** (crates/runtime/src/modules/_elementtree.rs): Element (Rust child
  vector, lazy attrib dict, emulated CPython `__sizeof__`, copy/deepcopy/
  `__getstate__`/`__setstate__` compatible with the Python Element pickles, mutation-safe
  remove/slicing/extend), `_element_iterator` (iter/itertext, non-recursive), a live
  child iterator, SubElement, `_set_factories`, TreeBuilder, XMLParser running the
  engine directly (fast path when the target is exactly our TreeBuilder; generic
  path otherwise; `_setevents` works for any target), ParseError, `_parse_whole`
  (used by ElementTree.parse for the default parser).  Fast path for find/findall/
  findtext with a plain tag; everything else goes to xml.etree.ElementPath.
* Engine perf: ASCII decode fast path, ASCII name table, boxed rare events, event
  vector pre-reservation.  ElementTree.write joins serializer output and writes once
  (except for tostringlist, which needs the pieces).
* **Lib/imaplib.py** (own design: buffered socket reader with IDLE deadlines,
  literal handling incl. stray blank lines, astring/list-mailbox quoting, UTF8=ACCEPT,
  Idler with burst()).  **Lib/xmlrpc/** (client, server, MultiPath/CGI/Doc servers;
  ServerHTMLDoc subclasses pydoc.HTMLDoc but has its own markup/docroutine/docserver).
* Tools: tools/markup2/{rt.sh,ut.sh,overlay.sh,bench_all.sh,etree_bench.py}.

## Test status (release build; overlay with http/urllib/pydoc where needed)

| test | result |
|---|---|
| test_xml_etree_c | 255 run, only failures = test_encoding CJK subtests; 10 skipped (CPython 7: +3 SizeofTest need `_testinternalcapi`) |
| test_xml_etree | 238 run, only CJK test_encoding subtests fail |
| test_pyexpat | 89 run, 4 skipped, only test_unsupported_encodings (CJK codecs) fail; test_exception passes |
| test_minidom / test_sax / test_pulldom / test_xml_dom_* | pass |
| test_imaplib | 239 run, pass (116 skipped: no `ssl` module) |
| test_xmlrpc | 94 run, pass (9 walltime skips like CPython; all pass with -u walltime) |
| test_docxmlrpc | 9/9 (needs pydoc + http from master) |

## Performance (pyperformance xml_etree, seconds per loop, noisy machine)

| | ours before | ours now | CPython best baseline |
|---|---|---|---|
| parse | 27.1 | ~0.8-1.2 | 0.40 |
| iterparse | 8.9 | ~0.7-1.2 | 0.22 |
| generate | 2.96 | ~0.36-0.49 | 0.20 |
| process | 1.42 | ~0.31 | 0.18 |

Profile notes: per parse of the 156 KB benchmark document ~31M instructions are the
engine (Vec<char> buffer, String allocation per event), ~13M tree building.  The
serializer is pure Python (as in CPython).  **Found outside my area:**
`TextIOWrapper.write` (crates/runtime/src/modules/io/textio.rs) spends most of its
time in `Decoder::reset` on every write (~4K instructions per call) — fixing that
would speed up all text-file writing (I worked around it in ElementTree.write).

## Next steps

* Engine: event batches → streaming callback, avoid `Vec<char>` (scan UTF-8 bytes
  directly), fewer String allocations for names (intern in the engine).
* Rule-9 review of minidom/mailbox/smtplib/ftplib/optparse **not done** (project paused):
  e.g. smtplib keeps untested internal helper names that mirror CPython
  (`_fix_eols`, `_quote_periods`, `bCRLF`, `OLDSTYLE_AUTH`, `_get_socket`,
  `_print_debug`, `_addr_only`); these should be redesigned.
* CJK codecs (not ours) are the remaining failures in the XML tests.
* imaplib/xmlrpc SSL paths are untested until `_ssl` lands.
