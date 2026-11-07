# cleanroom1 — D19 audit of Lib/ for recalled CPython text

Method: the three files flagged by tail1 (fractions, shlex, statistics) were rewritten
without opening the old version (spec tests + documented API only).  For the audit, files
were inspected for signals of recalled text (CPython's docstrings/comments verbatim, its
helper names and code layout beyond what tests pin); a file judged recalled was then
rewritten from the documented API and its spec tests, in a new structure, with own
docstrings/comments.  Private names that tests or the runtime use were kept (listed per
file).  "before/after" = the module's tests on an installed snapshot with the old file vs
the new file (same binary); "=" means identical results.

## Rewritten

| file | tests (before -> after) | names kept because pinned |
|------|-------------------------|---------------------------|
| shlex.py | test_shlex 45 ok -> 45 ok | (debug message texts asserted) |
| fractions.py | test_fractions 50 ok =, test_numeric_tower 9 ok (after rename), test_statistics, test_math = | `_hash_algorithm` (lru_cache, regrtest clears it), `_from_coprime_ints` (test_numeric_tower) |
| statistics.py | test_statistics 376 ok -> 395 ok (19 new own doctests), also with -u cpu | `_sum _coerce _convert _exact_ratio _fail_neg _isfinite _float_sqrt_of_frac _decimal_sqrt_of_frac _integer_sqrt_of_frac_rto _sqrtprod _kernel_specs _normal_dist_inv_cdf` |
| operator.py | test_operator 112 ok = ; test_inspect, test_pydoc = | (no module-level helpers allowed: test___all__ of test_operator counts every global with `__module__ == 'operator'`) |
| copy.py | test_copy, test_copyreg, test_dataclasses = ; test_descr, test_collections = (pre-existing failures) | `_deepcopy_dispatch` (test_descr), memo keep-alive under `memo[id(memo)]` |
| copyreg.py | test_copyreg, test_pickle (1085), test_copy = | `_reconstructor __newobj__ __newobj_ex__ _slotnames _reduce_ex _extension_registry _inverted_registry _extension_cache pickle_complex pickle_super` (runtime copyreg_support.rs + pickle) |
| abc.py, _py_abc.py | test_abc 76 =, test_typing, test_collections, test_dataclasses, test_pydoc, test_inspect = | `_abc_registry _abc_cache _abc_negative_cache _abc_negative_cache_version _abc_invalidation_counter _dump_registry _abc_registry_clear _abc_caches_clear` |
| linecache.py | test_linecache 36 =, test_traceback, test_inspect, test_warnings, test_zipimport, test_doctest = ; test_repl = (2 pre-existing failures) | `cache _interactive_cache _make_key _getlines_from_code _getline_from_code _register_code (runtime pymain.rs, _pyrepl) _bless_my_loader (runtime _warnings.rs)` |
| reprlib.py | test_reprlib 33 =, test_pprint =, test_collections = | `aRepr`, `repr_<type>` methods |

Behaviour changes (intended, all tests equal or better):
- linecache: `_bless_my_loader` delegates to importlib._bootstrap_external's (same rules);
  file text is split into lines on newlines only (str.splitlines also split on form feed
  etc., which shifted line numbers); zip members are read via zipimporter.get_data.
- fractions: hand-written literal/format-spec parsers (no `re` import); `Fraction.__float__`
  converts non-int numerator/denominator with int() first.
- statistics: NormalDist.samples uses random.gauss; kde caches the sorted data for
  bounded kernels and re-sorts when len(data) changes.
- shlex: no `collections`/`re` imports at all; pushback stacks are lists.

## Reviewed, judged clean (own text and structure)
(see audit list below)

## Still to do (judged recalled or suspicious, not yet rewritten)
(see audit list below)
