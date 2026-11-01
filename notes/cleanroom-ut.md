# Workstream "cleanroom-ut" — notes

Clean-room replacements (D19): the old files were deleted without being opened and
rewritten from the documented API plus what the spec tests observe.

## Files
* `Lib/unittest/` — `__init__` (lazy `IsolatedAsyncioTestCase`), `case`, `suite`, `loader`,
  `runner`, `result`, `main`, `signals`, `util`, `async_case`, `_log`, `__main__`.
* `Lib/unittest/mock.py` — new (there was none): Mock/MagicMock/NonCallable*, AsyncMock,
  ThreadingMock, PropertyMock, patch / patch.object / patch.dict / patch.multiple /
  patch.stopall, create_autospec, sentinel/DEFAULT, call/_Call/_CallList, ANY, mock_open,
  seal, FILTER_DIR, InvalidSpecError.
* `Lib/traceback.py`, `Lib/_colorize.py`, `Lib/argparse.py`.

## Design decisions (own)
* mock: per-instance subclasses (needed for magic methods); bookkeeping attributes behind a
  `_delegated` property factory (also registers the names that bypass spec_set checks);
  autospecced functions are closures (`_function_stand_in`), not exec'd source; `async def`
  stand-in for coroutine functions (signature checked when awaited). Normalised calls embed
  the call name in the arguments so differently named calls never match. Which spec attributes
  are coroutine functions is decided at construction from raw (static) attribute values, so
  properties never run.
* `_get_target()` resolves dotted names itself (no pkgutil dependency).
* unittest: `TestResult` methods avoid `super()` (tests copy them into an unrelated class);
  `assertWarns*` only lets the wanted warnings bypass the filters (a custom message matcher
  with `re.search` semantics is installed via `warnings._add_filter`) and re-emits everything
  else afterwards, so other filters (error/once/module) behave as without the context.
  Loader skips abstract TestCase classes (3.15). `doModuleCleanups` raises ExceptionGroup;
  the suite reports each member.
* traceback: marker rendering is a separate `_MarkedSource` class; display width handles East
  Asian wide chars, control chars (2 columns), combining marks (0) and ANSI escapes (0).
  Suggestions: cross-language table, mutable-on-immutable hints, float bitwise hint, NFKC
  matches ("instead of"), nested attribute ('.inner.value') search over the first 20 public
  attributes using dir() only. Keyword-typo hints use `SyntaxError._metadata` when present.
* argparse: help/usage built from per-action "parts"; mutually exclusive groups are placed at
  their first positional (else first) member; colour is applied after wrapping (wrapping is
  done on the plain text, `_recolor_lines` maps the colours back), so colour never changes
  line breaks; `` `literal` `` markup in colour mode; `_colorless_theme`; `__version__`
  deprecation via module `__getattr__`.
* `_colorize`: frozen dataclass sections (Argparse, Difflib, FancyCompleter, HttpServer,
  Syntax, Timeit, Traceback, Unittest).

## Runtime fixes made here (small, local)
* `types/list_methods.rs`: `list.__getitem__` on subclasses recursed forever (same fix landed
  on master meanwhile).
* `modules/io/stringio.rs`: `StringIO.__setstate__` works on an object created by `__new__`
  only (unpickling a TextTestRunner).
* `types/exceptions.rs`: `ImportError.__str__` uses `msg` only if it is an exact str
  (`str(ImportError(5))` raised TypeError).
* `compile.rs`: parser SyntaxErrors get `_metadata = (0, 0, source)` (keyword typo hints
  for `-c` and other non-file sources).

## Known gaps / not mine (found while testing)
* `test_unittest` as a package does not load: `test_assertions`/`testhelpers` need `datetime`,
  `testpatch` needs `types.LazyImportType` and `_testmultiphase`, `testthreadingmock` needs
  `concurrent.futures`. With a local datetime shim testhelpers passes 84/84.
* AttributeError raised by `__getattr__` without name/obj does not get `name`/`obj` filled in
  by the runtime (2 suggestion tests).
* Frames: `frame.f_back` is None after the frame returned (test_extract_stack); positions of
  instruction 0 lack columns; `-X no_debug_ranges` ignored; naked re-raise in `except*` has
  another line; Rust fallback printer prints source without io.open; tracebacks printed from
  `__del__` at shutdown missing.
* `python -c` does not dedent its argument (two traceback tests); running a directory /
  zipfile / .pyc as script fails in runpy (argparse prog-name tests).
* Lazy-import tests: `import re` imports `warnings` (enum/functools), `dataclasses` imports
  re/inspect/copy — argparse and _colorize lazy-import tests fail because of that.
* subprocess-based tests that need the `test` package with `-E`/`-I` cannot import it
  (environment: spec/Lib is only on PYTHONPATH).
