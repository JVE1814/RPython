# cleanroom1 — D19 audit of Lib/ for recalled CPython text

## Method
- The three files flagged by tail1 (fractions, shlex, statistics) were rewritten without
  opening the old version: spec tests + documented API only.
- Audit signals: the grep from the assignment (`bpo-|gh-NNN|Issue #|Copyright|Written by|XXX`)
  finds nothing relevant in Lib/ any more (only false positives: `\uXXXX`, `pymp-XXXXXXXX`,
  an idlelib "Copyright" button, a cookiejar format comment).  So the audit was done by
  reading: module docstrings of every top-level module > 120 lines and of the json, http,
  urllib, email, importlib, concurrent.futures, logging, wsgiref, xmlrpc, sqlite3, dbm and
  string packages; then bodies of modules whose CPython source has distinctive text, and a
  grep for CPython-internal helper names (see "still to do").
- A file judged recalled was rewritten from the documented API and its spec tests in a new
  structure, with own docstrings/comments.  Private names that tests or the runtime use were
  kept (listed per file).
- "before -> after" = the module's spec tests on an installed snapshot with the old file vs
  the new file, same binary (target/cleanroom1); "=" means identical counts/results.
  Helper scripts used: a `prep.sh` that puts `git show HEAD:Lib/<f>` into the "before"
  snapshot and the working file into the "after" snapshot.

## Rewritten (whole module, own structure and text)

| file | tests before -> after | names kept because pinned (tests/runtime) |
|------|-----------------------|-------------------------------------------|
| shlex.py | test_shlex 45 ok = | debug message texts (asserted); `state` values `' '`/`'a'`/`'c'` |
| fractions.py | test_fractions 50 ok =; test_numeric_tower 9 =; test_math, test_statistics, test_random, test_float, test_complex = | `_hash_algorithm` (lru_cache; regrtest calls cache_clear), `_from_coprime_ints` (test_numeric_tower) |
| statistics.py | test_statistics 376 ok -> 395 ok (19 extra own doctest examples); also = with `-u cpu` | `_sum _coerce _convert _exact_ratio _fail_neg _isfinite _float_sqrt_of_frac _decimal_sqrt_of_frac _integer_sqrt_of_frac_rto _sqrtprod _kernel_specs _normal_dist_inv_cdf` |
| operator.py | test_operator 112 =; test_inspect, test_pydoc = | none; note test_operator's `test___all__` counts *every* module global with `__module__ == 'operator'`, so no module-level helper functions/classes are allowed |
| copy.py | test_copy 84 =, test_copyreg, test_dataclasses =; test_descr, test_collections = (pre-existing failures) | `_deepcopy_dispatch` (test_descr), keep-alive list at `memo[id(memo)]` |
| copyreg.py | test_copyreg 6 =, test_pickle 1085 =, test_copy = | `dispatch_table _reconstructor __newobj__ __newobj_ex__ _slotnames _reduce_ex _extension_registry _inverted_registry _extension_cache pickle_complex pickle_super` (crates/runtime/src/modules/copyreg_support.rs, pickle) |
| abc.py, _py_abc.py | test_abc 76 =; test_typing 731, test_collections, test_dataclasses, test_pydoc, test_inspect = | `_abc_registry _abc_cache _abc_negative_cache _abc_negative_cache_version` (libregrtest/refleak.py), `_abc_invalidation_counter`, `_dump_registry _abc_registry_clear _abc_caches_clear` |
| linecache.py | test_linecache 36 =; test_traceback 441, test_inspect, test_warnings, test_zipimport, test_doctest =; test_repl = (2 pre-existing failures) | `cache _interactive_cache _make_key _getlines_from_code _getline_from_code _register_code` (pymain.rs, _pyrepl) `_bless_my_loader` (_warnings.rs) |
| reprlib.py | test_reprlib 33 =, test_pprint, test_collections = | `aRepr`, `repr_<type>` methods |
| collections/__init__.py | test_collections 120 run/3 fail -> 110 run/1 fail (fewer doctest examples; the Counter in-place/xor failures are fixed, the remaining failure is in _collections_abc); test_ordered_dict 299, test_userdict, test_userlist, test_userstring =; test_typing, test_pprint, test_enum, test_functools, test_pydoc, test_configparser, test_json, test_pickle = | `_count_elements`, `_tuplegetter`, `_deque_iterator`, `abc`; pure OrderedDict nodes have a `key` attribute (test_issue119004_attribute_error asserts the AttributeError message) |
| fnmatch.py | test_fnmatch 24 =, test_glob, test_pathlib 1398 = | `_translate(pat, star, question_mark) -> (parts, star_indices)` (test_fnmatch, glob.py) |
| bisect.py | test_bisect 46 = | — (insort computes the index before `a.insert`, tests check the error type) |
| _weakrefset.py | test_weakset 46 =, test_weakref 140 =, test_abc, test_threading = | `data` (repr test), guard protocol `_iterating/_pending_removals/_commit_removals` and the alias `_IterationGuard` (still imported by the old weakref.py); `__reduce__` must carry the full instance state incl. `data` (test_copying relies on deep copies sharing the elements) |

Text-only fixes (code was already the agents' own or is trivial): heapq.py (module docstring
was CPython's usage table; `__about__` dropped), queue.py (Empty/Full docstrings),
posixpath.py (header comment), glob.py, importlib/abc.py, _py_warnings.py (module docstrings).

Broad regression check after all of the above (before = master Lib, after = this branch):
test_unittest 1102, test_argparse 1963, test_logging 289, test_tempfile 121, test_shutil 227,
test_posixpath 96, test_email 1818, test_csv 140, test_string 55, test_textwrap 68,
test___all__ (3 pre-existing failures: curses, multiprocessing.managers, os), test_importlib
(6 pre-existing errors), test_queue 165, test_heapq 69 — all identical.

### Behaviour changes (intended; all tests equal or better)
- linecache: `_bless_my_loader` delegates to importlib._bootstrap_external's (same rules);
  file text is split into lines on newlines only (str.splitlines also split on form feeds
  etc., which shifted line numbers of such files); zip members are read with
  `zipimporter.get_data`.
- collections.Counter: `+= -= |= &= ^=` and `^` now follow the multiset rules for every
  element (previously wrong for some negative/zero counts, see test_collections).
- fractions: hand-written literal and format-spec parsers (no `re` import); `__float__`
  converts non-int numerator/denominator with int() first (test_numeric_tower).
- statistics: NormalDist.samples draws with random.gauss; kde caches the sorted data for
  bounded kernels and re-sorts when len(data) changes; stdev/pstdev raise ValueError for
  data containing inf/nan.
- shlex: imports neither collections nor re; pushback stacks are lists.
- Counter.fromkeys message, a few error messages of copyreg/linecache helpers are own text.

## Reviewed: own text and structure (no action)
graphlib.py, sched.py, heapq.py (code), numbers.py (structure is dictated by PEP 3141; only
the first docstring line matches CPython — low priority), the module docstrings of all other
top-level modules > 120 lines and of the packages listed under Method (own wording).
Earlier clean-room work kept as is: _collections_abc, enum, dataclasses, contextlib
(cleanroom-lang); inspect, logging/*, tempfile, shutil (cleanroom-sys); unittest/*, mock,
traceback, _colorize, argparse (cleanroom-ut); importlib/util.py.

## Still to do (recalled code/text found, not rewritten yet) — priority order
1. functools.py — lru_cache docstring is CPython's verbatim; internals use CPython's names
   and layout (`_HashedSeq _make_key _lru_cache_wrapper _c3_merge _c3_mro _PlaceholderType
   _unwrap_partial _singledispatchmethod_get`).  Pinned by tests: `_find_impl`,
   `_compose_mro`, `_CacheInfo`; `_functools` replaces partial/reduce/lru wrapper when present.
2. weakref.py — CPython skeleton with paraphrased docs (KeyedRef, Weak*Dictionary internals,
   finalize `_registry/_Info/_exitfunc/_select_for_exit`).  Pinned: KeyedRef (public);
   `WeakValueDictionary()._remove.__closure__ is None` (cpython_only test).  When rewritten,
   drop the `_IterationGuard` alias from _weakrefset.py (or keep the guard protocol).
3. pickletools.py — opcode documentation and 25 doctests; very likely recalled text (not
   inspected in detail).
4. glob.py — CPython structure (`_iglob`, `has_magic`, `_isrecursive`, `_lexists`, `_join`,
   translate) with paraphrased docstrings.
5. types.py — CPython internals (`_cell_factory`, `_GeneratorWrapper`, `_calculate_meta`).
6. tokenize.py (`group/any/maybe` regex builders), pprint.py (`_safe_key`, `_safe_tuple`,
   `_recursion`), difflib.py (`_mdiff`, `_count_leading`, `_file_template`), calendar.py
   (`_localized_month/_localized_day`, `different_locale`), string/__init__.py
   (`_sentinel_dict`, `_ChainMap`), urllib/parse.py (`_coerce_args`, `_splitparams`, `_noop`),
   http/cookies.py (`_unquote`, `_quote`, `_is_legal_key`, `_LegalChars`),
   posixpath.py/ntpath.py/genericpath.py (`_get_sep`, `_joinrealpath`, `_check_arg_types`,
   `_splitext`) — CPython helper names surviving; check which ones tests pin before
   renaming.
7. queue.py — control flow mirrors CPython (the `_init/_qsize/_put/_get` hooks and
   `mutex/not_empty/not_full/all_tasks_done/unfinished_tasks` attributes are API).
8. Not audited at all: bodies of the large modules (pydoc, tarfile, zipfile, mailbox,
   http/*, urllib/request, email/_header_value_parser, xml/*, asyncio/*, multiprocessing/*,
   tkinter/*, idlelib/*, _pydecimal, _pydatetime, _pyio, typing, doctest, pickle, ...).
   Avoided on purpose (other agents active): pdb.py, re/*, zoneinfo/*.
