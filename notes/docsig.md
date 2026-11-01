# docsig workstream — `__text_signature__` for builtins

## Mechanism (unchanged, documented here)
* A builtin's signature lives in its docstring: `"name(SIG)\n--\n\nDoc text"`.
  `__text_signature__` returns `SIG`, `__doc__` the rest (None if empty).
  For methods SIG starts with `$self`, classmethods `$type`, module functions
  `$module` (only when the function is bound to its module; functions created
  with a NULL self, e.g. most of `sys`, must not use `$module`).
* Types: `tp_doc = "TypeName(SIG)\n--\n\n..."` (TypeName = last component of tp_name).
* Unrepresentable defaults are written `<unrepresentable>`; optional groups as
  `[, x]`.  inspect cannot parse either: that is how CPython marks the callables
  the tests list as "unsupported" (dict.pop, str.count, anext, os.chmod, ...).
* Callables CPython has *no* signature for (dict.update, min/max, getattr, range,
  int, str, ...) keep a docstring without the `--` marker.

## Decisions
* Implied signatures (descr.rs `flags_text_signature`, used by method
  descriptors and builtin functions): a METH_NOARGS / METH_O callable whose doc has
  no signature reports `($self, /)` / `($self, object, /)` (`$type` for class
  methods, `$module` for module functions, nothing for NULL-self/static). This is
  what test_inspect/test_pydoc expect (meth_noargs / meth_o templates).  A doc
  that starts with `name(` but has no `--` block (a legacy "name(args) -> x"
  line) suppresses the implied signature: CPython has none for those either
  (e.g. _socket, BufferedRWPair.readinto).
* `__new__` of builtin types: `($type, *args, **kwargs)`.
* Types flagged Py_TPFLAGS_DISALLOW_INSTANTIATION no longer get a `__new__` in
  their dict (slots.rs), so inspect falls back to object's signature like CPython
  (weakref.ProxyType, _csv.reader/writer).
* datetime: native types got constructor signatures; `replace`/`combine`
  (pure-Python functions copied onto the native types) get private copies
  carrying an unparsable `__text_signature__`, matching the C module.
  `tzinfo` now uses a generic tp_new (its own `__new__`, no signature).
* `functools.reduce` is `($module, function, iterable, /, initial=None)` so it
  is introspectable (test_functools_module_has_signatures requires it).
* Lib/inspect.py: `_signature_strip_non_python_syntax` kept `b''` prefixes
  apart from the string (`b ''`), breaking every bytes default. Fixed.
* Docstrings written while adding signatures are our own words (D19); where
  test_doctest counts examples (builtins: int, int.bit_length/bit_count/
  as_integer_ratio, bytes/bytearray/memoryview.hex) the docs carry the required
  number of `>>>` examples, with our own values.

## Tools
* `tools/sigcheck.sh [PY] [--all] [--verbose] [modules]` runs
  `tools/sigcheck.py` with our interpreter: walks builtins and the modules of
  test_inspect's `test_*_module_has_signatures` (with the same
  no-signature/unsupported expectation tables) plus extra builtin modules and
  reports MISSING / EXTRA / BADSIG.  Legacy-doc and `<unrepresentable>` cases in
  non-test modules are CPython parity and only shown with --verbose.
* `tools/docsig_subst.rs` (rustc -O -o /tmp/docsig_subst tools/docsig_subst.rs):
  rule-driven editor used to add docstrings to `mdefs!` entries
  (`= name[@func]|SIG|DOC`) and literal replacements.

## State
See the final report in the workstream branch log; remaining known gaps:
* collections.OrderedDict is the pure-Python class (no C OrderedDict), so
  OrderedDict / OrderedDict.update have signatures and OrderedDict.pop is not
  "unsupported" (3 sigcheck entries).
* select.epoll.register's default `select.EPOLLIN | ...` does not evaluate (the
  name `select` in that module is the function).

## Status at pause (2026-11-01)
* sigcheck: 5985 callables checked, 9 problems left (3 OrderedDict, tracemalloc
  modules absent, _lzma absent, _json.make_scanner and _contextvars.Token have
  no signature in CPython either, select.epoll.register default).
* test_inspect: all `test_*_module_has_signatures` pass except collections
  (OrderedDict is pure Python) and tracemalloc (module missing).  Remaining
  test_inspect failures (signature_on_class*, callable_objects, getsource,
  getfullargspec, CLI details, ...) are in Lib/inspect.py logic, not docs.
* test_doctest builtins example counts: docs updated (int, int.bit_length,
  bit_count, as_integer_ratio, bytes/bytearray/memoryview.hex); not re-verified
  after the last build.
## Next steps
* Native C-style OrderedDict in _collections (or accept the 3 gaps).
* Fix Lib/inspect.py class-signature logic (test_signature_on_class*,
  test_builtin_descriptors, getfullargspec on builtins).
* `tools/sigcheck.sh --all` to cover private names of every builtin module;
  add docstrings to the many METH_NOARGS functions that only have the implied
  signature (os.getpid, time.time, ...).
* Rewrite remaining CPython-recalled docstrings (D19) in files touched here
  (dict, range, socket, struct, _functools.reduce text, ...).
