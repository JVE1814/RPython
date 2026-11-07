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

## Reviewed: own text and structure, no action needed
(none yet)

## Still to do — priority order
1. difflib.py, 2. posixpath/ntpath/genericpath, 3. urllib/parse.py, 4. _pydatetime, pydoc,
typing, doctest, tarfile, zipfile, 5. bodies never audited: mailbox, http client/server,
urllib/request, email, xml, asyncio, multiprocessing, _pyio, pickle (see cleanroom2's list for
the helper names involved).
