# Workstream `str` — notes and decisions

## Layout of the code
* `crates/runtime/src/types/str_methods.rs` — the `str` type: all slots (repr, str, hash,
  richcompare, sequence/mapping, iteration, `nb_remainder` = `%`, `tp_new`, `tp_dealloc`),
  every method, the `str_ascii_iterator`/`str_iterator` types and the `_string` module
  (`init_string_module`, registered in `modules::register_all`; CPython also defines
  `_string` next to the str type).
* `str_methods/search.rs` — substring search over UCS1/2/4 slices: single-unit scan
  (SWAR memchr for UCS1), Horspool + 64-bit bloom mask for the common case, switching
  to Crochemore–Perrin two-way when verification work exceeds a linear budget (worst
  case O(n+m), also for rfind/rsplit/rpartition via reversed views). Unit tests compare
  against a naive search and check adversarial inputs.
* `str_methods/writer.rs` — width-adaptive `Writer` (u8/u16/u32 buffers, widening on
  demand, canonical result), exact-size construction helpers, fallible allocation
  (`try_alloc` → MemoryError), `substring` (identity for full exact strings).
* `str_methods/case.rs` — full case mappings, Final_Sigma, predicates via rpy-unicode with
  ASCII / Latin-1 fast paths.
* `str_methods/format.rs` — `str.format`/`format_map` engine (markup iterator, field-name
  iterator, auto/manual numbering, conversions, nested specs with depth 2) and the
  `_string` helpers `formatter_parser` / `formatter_field_name_split` (pub Rust fns).
* `str_methods/percent.rs` — printf-style `%`; specs parsed by `rpy_numfmt::percent`,
  numbers rendered by `percent_format_int/float`.
* `types/str.rs` gained `try_alloc_str` (fallible variant of `alloc_str`).
* `format_impl::format_str` now delegates to `str_methods::format_str_spec` (PEP 393 aware,
  no lossy conversion).

## Decisions
* `%` ordering: like CPython, the argument count is checked (and the value fetched)
  before a bad conversion character is reported (`'%s%z' % (x,)` → TypeError), and
  `*` arguments are counted before their type is checked.
* After a `%(key)` conversion, a positional conversion raises
  "format requires a parenthesised mapping key at position N" (3.15 test expectation).
  A positional conversion *before* any keyed one still consumes the mapping itself
  (old CPython behaviour).
* `str.maketrans` is a METH_STATIC method with its own C shim (the generic method shims
  dereference `self`, which is NULL for static methods).
* Iterator types have `tp_new` cleared after `type_ready` (not instantiable).
* Identity optimisations (cpython_only tests): `s.format()`, `'%s' % s`, `'{}'.format(s)`,
  `s.replace(x, x)`, `s.expandtabs()` without tabs, `s * 1`, strip/split/partition
  results return the exact str itself where CPython does.

## Testing
No unittest/regrtest yet in master at the time of writing; a private shim (stub
`unittest`, `test.support`, etc. under /tmp) was used to run the spec classes directly:
* test_str: 111 pass; remaining failures are outside this workstream (codecs other than
  utf-8/ascii/latin-1, `memoryview`, `bytes.fromhex`, `enum`, `datetime`, `_testcapi`).
  Two tests trip a runtime bug elsewhere (see below).
* test_format: every str case passes (bytes/bytearray cases belong to the bytes workstream).
* test_unicode_identifiers: pass.

## Known issues found outside this workstream
* Subclassing a *Python* class that defines `__getitem__`/`__init__` and overriding
  `__init__` in the subclass gives infinite recursion (`class A: __getitem__...;
  class B(A): def __init__...; B()[0]` → RecursionError). Hits string_tests
  `LiesAboutLengthSeq` and test_str's `I(datetime.date)`.
* A class inheriting from several bases where `__init__` comes from a later base
  (`StrTest(StringLikeTest, ..., TestCase)`) reports "takes no arguments".
* List slicing with huge indices overflows (core_slots list subscript; test_extended_getslice).
* Literal subscripts like `'abc'[1.5]` make the compiler emit a SyntaxWarning, and the
  warnings machinery then fails (`_warnings_context` is None).

## Missing / not done
* `string` module (Lib/string.py, `string.Formatter`) and `collections.UserString` are not
  part of this workstream; test_string / test_userstring could not be run.
* Codecs other than utf-8/ascii/latin-1 are delegated to the codec registry.
