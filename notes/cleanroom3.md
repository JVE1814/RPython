# cleanroom3 — continuation of the D19 audit (see notes/cleanroom1.md, notes/cleanroom2.md)

## Method
- Same as the predecessors: a module flagged as recalled is rewritten without reading the old
  body (only its list of top-level names, to learn what exists), from the documented API, the
  spec tests and a grep of crates/ + Lib/ + spec/Lib/test (subpackages included) for every
  private name other code uses.
- "before -> after": the module's spec tests on an installed snapshot
  (target/cleanroom3/snap) with the old file and with the new file, same binary.
- New for pprint: a differential fuzzer (random nested containers x all option combinations,
  old module vs new module, outputs and saferepr/isreadable/isrecursive compared) to find
  behaviour the tests do not pin.  Scratch scripts lived in an untracked `.scratch3/` in the
  worktree (the Write tool may not write under target/).

## Rewritten

| file | tests before -> after | names/texts kept because pinned |
|------|-----------------------|---------------------------------|
| pprint.py | test_pprint 77 ok =; test___all__, test_inspect, test_pydoc, test_unittest, test_descrtut, test_sys_setprofile, test_htmlparser, test_dataclasses, test_support, test_trace =; test_collections = (1 pre-existing failure, _collections_abc) | `PrettyPrinter._safe_repr(object, context, maxlevels, level)` (pyperformance bm_pprint benchmarks it when present), `PrettyPrinter.format` (subclass hook, test_pprint's DottedPrettyPrinter), recursion marker text `<Recursion on T with id=N>`, the generated-dataclass check (`__wrapped__.__qualname__` containing `__create_fn__`, which Lib/dataclasses.py produces on purpose) |

### pprint.py notes
- New structure: one-line texts come from a table of module-level `_flat_*` functions
  (`_FLAT`, looked up by `_lookup`: exact type, else the nearest registered base when the
  subclass kept that base's `__repr__`); layouts are `_lay_*` methods in `_LAYOUTS` (same
  lookup).  The layouts are built from four shapes: `_values` (bare items, with compact
  packing in `_packed`), `_pairs` (key: value), `_fields` (name=value), and `_expanded`
  (the expand=True form of all three); `_chunks`/`_text_chunks`/`_bytes_chunks` cut str and
  bytes literals.  Output is collected with `list.append` (pformat) or `stream.write`.
- Expand mode: in expand mode nested blocks are indented from the start of the line, so
  layouts carry `lead` (characters already on the line) for the width check; strings/bytes
  under a key are cut at the block column (pinned by test_expand_template).
- Speed: `_parts` dispatches child objects directly (no format()/_safe_repr() call per child)
  when the printer uses the stock hooks; dict sorting first tries a plain key sort and only
  falls back to the tolerant `_OrderingKey` on TypeError (same order when nothing fails);
  `_emit` keeps the item texts of plain lists/tuples/dicts (`_SPLIT`) so a container spread
  over several lines does not format its items again.  Numbers (ms, same binary, noisy box):
  bm_pprint data `_safe_repr` 600 -> 430, `pformat` 1300 -> 460; a 3000-entry nested dict
  pformat 48 -> 19, with width=40/compact 87 -> 40.  pyperformance runner (bench shim):
  pprint 0.599 s -> 0.427 s.
- Classic-mode quirks kept on purpose (differential fuzzing against the old file, 20k cases,
  0 differences without expand): a depth-truncated 1-tuple is `(...,)`; the last
  name=value field of a namespace/dataclass/Template reserves no room for its own `)`; an
  abc MappingView's mapping keeps the view's column and reserves no room for `)`; dict
  views (`dict_keys([` ...) indent their items by one step only; bytes reserve the trailing
  allowance only for a final group shorter than four bytes.
- Behaviour changes (all tests equal):
  - saferepr()/one-line text of a frozendict (and subclasses that keep its `__repr__`) is
    built like a dict's (entries sorted when sort_dicts, depth/recursion aware) instead of
    plain repr(), so short and multi-line output agree.
  - expand=True: empty bytearray / empty namespace / empty containers print as their repr
    instead of an opener and closer on two lines; an object truncated by `depth` inside a
    keyed entry, a UserDict or an OrderedDict is judged by the same width rule as anywhere
    else (the old file sometimes expanded it, sometimes not).
  - The unused `_readable`/`_recursive` instance attributes are gone; private helpers
    (`_safe_key`, `_safe_tuple`, `_recursion`, `_wrap_bytes_repr`, `_dispatch`,
    `_pprint_*`, `_format*`) no longer exist (nothing in Lib, crates or spec uses them).
  - Validation messages are own wording (no test asserts them).

### difflib.py (rewritten)
- test_difflib 41 ok -> 54 ok (13 more own doctest examples; the byte-exact
  test_difflib_expect.html comparison passes); test_unittest, test_doctest, test_argparse,
  test_genericalias, test___all__, test_pydoc, test_inspect, test_traceback = (all pass).
- Kept (pinned): `_mdiff(fromlines, tolines, context=None, linejunk=None, charjunk=...)` and
  its row format, `_format_range_unified`, `_format_range_context`, `HtmlDiff._default_prefix`
  (test sets it), every message test_difflib asserts, the HTML markup (taken from the expected
  file), the SequenceMatcher attributes `a b isjunk autojunk b2j bjunk bpopular`, and the
  HtmlDiff class attributes `_file_template _styles _table_template _legend` (the known way to
  restyle the output).  Dropped: `__chain_b`, `_fancy_replace/_fancy_helper/_plain_replace/
  _qformat/_dump`, `_keep_original_ws`, `_calculate_ratio`, `_check_types` stays (own body),
  `HtmlDiff._split_line/_line_wrapper/_collect_lines/_format_line/_make_prefix/
  _convert_flags/_tab_newline_replace`, `fullbcount`, `matching_blocks`/`opcodes` attributes.
- New structure: `_RowBuilder` turns the ndiff delta into side-by-side rows with an ordered
  rule table (lookahead code prefix -> action) and two cell queues; `_context_rows` keeps
  `context` rows around changes; `_wrap_point`/`HtmlDiff._split` wrap marked-up text;
  `HtmlDiff._navigation` builds the anchors/links.  Differ pairs similar lines by scanning the
  b lines in order with a +-10 line window (`_similar_replace`), no recursion.
- Differential fuzzing against the old file (1500 random line/character inputs x
  SequenceMatcher opcodes/grouped/ratios, ndiff, unified, context, get_close_matches, HtmlDiff
  with/without context and wrapping): 0 differences.
- Behaviour change: get_grouped_opcodes() no longer rewrites the first/last entries of the
  cached get_opcodes() list (it works on a copy).  Timing ~equal (opcodes/ndiff/unified on a
  1000-line file, char ratio, HtmlDiff).

### posixpath.py, genericpath.py (rewritten); ntpath.py (reviewed, references updated)
- test_posixpath, test_genericpath, test_ntpath 232 run / 31 skipped = (all pass);
  test_pathlib, test_shutil, test_tempfile, test_glob, test_import, test_site,
  test_zipimport, test_inspect, test_pydoc, test___all__, test_unittest =; test_importlib
  (6 errors) and test_os (urandom EMFILE check) fail the same with the old files.
- realpath: own design — a work stack of components; a symlink pushes its target's
  components plus a `_Finished` marker that records the link's resolution in `seen` (None
  while in progress = loop); the kind of the resolved prefix ('dir'/'other'/unknown) gives
  NotADirectoryError for "file/", "file/." and "file/.." in the strict modes; ALL_BUT_LAST
  tolerates a missing component only when nothing but ''/'.' follows.  Loops: ELOOP OSError
  in strict modes, the link kept literally (rest still processed) in lenient mode.
- ismount: lstat of path/.. first, realpath(path/..) only when that fails.
- genericpath: helpers renamed `_splitext` -> `_split_extension`, `_check_arg_types` ->
  `_reject_mixed_types` (ntpath updated), `_commonprefix` -> `_common_start` (zip-based
  instead of the min/max trick), `_RealpathMode` -> `_StrictMode` (repr/pickle/copy as the
  tests require).  Nothing in Lib/crates/spec used the old private names.
- Pinned and kept: `posix._path_splitroot_ex`/`_path_normpath` are used as splitroot/normpath
  when present (test_fast_paths_in_use); Python fallbacks `_splitroot`/`_normpath` are own.
  All messages tests assert ("Can't mix strings and bytes in path components", "no path
  specified", "Can't mix absolute and relative paths", the commonprefix deprecation must
  mention "commonpath()").
- Differential fuzzing against the old files: 13,500 random str/bytes paths through
  split/splitext/basename/dirname/isabs/splitdrive/normcase/abspath/normpath/splitroot/
  join/relpath/commonpath/expandvars/expanduser, and 600 random symlink trees x 20 paths x
  4 strict modes x str/bytes through realpath (+ ismount/islink/exists/isdir/isfile/lexists):
  0 differences (after fixing join's check order: `part.startswith(sep)` must run before
  `not path` so join('', b'x') still raises TypeError).
- Timing (ns/call, same process, interleaved): join 500 = 500, split 430 -> 420, dirname
  360 -> 350, splitext ~430 =, abspath ~385 =.
- ntpath.py verdict: own structure already (`_Vocab`, own splitroot/join/expandvars scanner,
  `_emulated_abspath`, own isreserved tables); only the genericpath helper names changed.

### urllib/parse.py (partial rewrite: the CPython-shaped helpers)
- Verdict: result classes, splitting/joining core, quoting API and query parsing were
  already own design (`_KeepEmpty`, `_make_result`, `_split_cached/_split_impl`, `_unsplit`,
  `_deprecated_split`); the CPython-shaped pieces were replaced:
  `_noop/_encode_result/_decode_args/_coerce_args` -> `_texts()` returning (str args,
  was_bytes); `_coerce_components` -> `_component_texts`; `_splitparams` -> `_cut_params`
  (rpartition/partition); `_splitnetloc` -> `_take_authority` (regex search for the first
  '/', '?' or '#'); `_checknetloc` -> `_reject_lookalike_netloc`; `_check_bracketed_*` ->
  `_validate_bracketed_*`; `_remove_dot_segments` -> `_drop_dot_segments`;
  `_hextobyte/_hex_table` -> `_escape_table()` (all four hex-case spellings);
  `_byte_quoter_factory` -> `_quoter_for`; quote_from_bytes' all-safe fast path uses
  `bytes.translate(None, safe)`; `_parse_qsl` -> `_query_pairs`; `_ASCII_RUN` ->
  `_ASCII_SPANS`; `_HOST_RE/_PORT_RE` dropped (`_splithost` via `_take_authority`,
  `_splitport` via rpartition; a trailing ':' without digits is still dropped).
- Kept (pinned by test_urlparse): `_Quoter` (repr contains 'Quoter'), `_ALWAYS_SAFE`,
  `_to_bytes`, the `_split*` family and their deprecated public wrappers, `__getattr__`,
  `clear_cache`, `to_bytes`, `unwrap`, `_encoded_counterpart/_decoded_counterpart`,
  "Cannot mix str and non-str arguments", the port messages.
- Tests: test_urlparse 1 failure on the first try (`_splitport('parrot:')` must give
  ('parrot', None)) -> fixed; then test_urlparse, test_urllib2, test_robotparser,
  test_http_cookiejar, test___all__, test_pydoc, test_genericalias, test_xmlrpc,
  test_mimetypes, test_pathlib = (pass); test_urllib 2 errors (host control-char checks
  in http.client) fail the same with the old file.
- Differential fuzzing (13,000 random str/bytes URLs through every public function, the
  result attributes, geturl/urlunsplit round trips and the `_split*` helpers): 0
  differences, except that `_splithost(bytes)` now raises TypeError explicitly (the old
  regex raised TypeError too, with another message).  Timing ~equal (noisy box).

### doctest.py (partial rewrite: module-level helpers)
- Verdict: classes (Example, DocTest, DocTestParser, DocTestFinder, OutputChecker,
  DocTestRunner, DocTestCase, ...) are a condensed own version (1730 lines); the module
  helpers kept CPython's names and some of its bodies.  Renamed and rewritten:
  `_extract_future_flags` -> `_future_flags_of`, `_normalize_module` -> `_resolve_module`
  (importlib.import_module), `_newline_convert` -> `_universal_newlines`, `_load_testfile`
  -> `_read_test_file`, `_indent` -> `_indented` (split on '\n', no regex),
  `_exception_traceback` -> `_format_exc_info`, `_SpoofOut` -> `_CapturedOutput`,
  `_ellipsis_match` -> `_matches_with_ellipsis`, `_comment_line` -> `_as_comment`,
  `_strip_exception_details` -> `_bare_exception_name`, `_OutputRedirectingPdb` ->
  `_redirecting_debugger` (the placeholder class is gone), `_module_relative_path` ->
  `_path_beside_module`, `_unittest_reportflags` -> `_unittest_report_flags`,
  `_DocTestSuite` -> `_SuiteOfDocTests`, `_test` -> `_main`.  Nothing outside doctest.py
  used them.  Kept: the `DocTestCase._dt_*` attributes (third-party test runners read
  `_dt_test`), every public name, the module-relative path error messages.
- test_doctest 60 =, test_difflib, test_pprint, test_statistics, test_unittest,
  test_zipimport_support, test_cmd, test_code, test_fractions = (pass); test_collections
  (1) and test_pdb (pdb1's rewrite in progress) fail the same before and after.

### pydoc.py — reviewed, own
- Own structure throughout (`_own_docstring`, `_owner_class`, `_routine_summary`,
  `_class_groups`, `_signature_text`, ...).  The CPython-looking names cleanroom2 counted
  (`pathdirs`, `cram`, `stripid`, `classname`, `isdata`, `allmethods`, `visiblename`,
  `classify_class_attrs`, `sort_attributes`, `ispackage`, `source_synopsis`, `synopsis`,
  `importfile`, `safeimport`, `locate`, `replace`, `splitdoc`) are module-level public
  functions that test_pydoc and third-party code call; their bodies are own.  No action.

### zipfile/__init__.py (partial: CPython helper names and the cipher)
- Renamed (nothing outside the module used them): `_EndRecData64` -> `_merge_zip64_end`,
  `_EndRecData` -> `_find_end_record`, `_check_zipfile` -> `_looks_like_zip` (body
  rewritten), `_strip_extra` -> `_without_extra_ids`, `_sanitize_filename` ->
  `_clean_member_name`, `_SharedFile` -> `_ArchiveView`, `_Tellable` -> `_CountingWriter`,
  `_ZipWriteFile` -> `_MemberWriter`, field-index constants `_ECD_*/_CD64_*/_CD_*/_FH_*` ->
  `_END_*/_END64_*/_CEN_*/_LOC_*`.  ZipCrypto: `_crctable/_gen_crc/_ZipDecrypter` replaced
  by `_reflected_crc_table()` and the `_ZipCryptoDecoder` class (keys in a list, chunked
  calls keep state); verified byte-identical to the old decrypter on 500 random
  password/data/chunking cases, speed ~equal.
- Kept (pinned): `_Extra`, `_get_compressor`, `_get_decompressor`, `_check_compression`,
  `_MASK_USE_DATA_DESCRIPTOR`, the public struct names/sizes (`sizeEndCentDir*`,
  `sizeCentralDir`, `struct*`, `string*`), `_path`, every message.
- Not done: ZipInfo/ZipExtFile/ZipFile bodies keep CPython's decomposition (method names
  are largely API); a full rewrite would be the next step.
- test_zipfile, test_zipfile64, test_zipimport, test_zipapp, test_shutil, test___all__ =.

## Reviewed: own text and structure, no action needed
(none yet)

## Still to do — priority order
1. difflib.py, 2. posixpath/ntpath/genericpath, 3. urllib/parse.py, 4. _pydatetime, pydoc,
typing, doctest, tarfile, zipfile, 5. bodies never audited: mailbox, http client/server,
urllib/request, email, xml, asyncio, multiprocessing, _pyio, pickle (see cleanroom2's list for
the helper names involved).
