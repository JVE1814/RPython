# lib6 notes (stdlib failure grind, after suite run 7)

## Results (verbose runs on an installed-layout snapshot)
| module | run 7 | before (master 7cac9a5) | after |
|---|---|---|---|
| test_importlib | 39 | 6 | 1 |
| test_itertools | crash | crash (count race) + 9 | 0 |
| test_pathlib | 6 | 6 | 0 |
| test_import | 6 | 17 (incl. errors) | 3 failures (+ subinterpreter errors) |
| test_runpy | 6 | 6 | 0 |
| test_sqlite3 | 5 | 5 | 0 |
| test_re | 5 | 5 | 4 |
| test_struct | 4 | 10 (incl. errors) | 1 error |
| test_string | 3 | 6 | 0 |
| test_syslog | 3 | 4 | 4 |
| test_zipfile | 3 | 4 | 0 |
| test_unittest | 3 | 12 | 0 |
| test_pkg | 3 | 0 | 0 |
| test_utf8_mode | 4 | 4 | 0 |
| test_c_locale_coercion | 4 | 4 | 0 |
| test_atexit | 4 | 4 | 4 |
| test_audit | 4 | 4 | 1 |
Side effects: test_range now passes (range iterator overflow).

## What remains, and why
* Subinterpreters (no `_interpreters`, `run_in_subinterp` runs in the main interpreter):
  test_atexit SubinterpreterTest (4), test_syslog test_subinterpreter_* (4),
  test_import SubinterpImportTests (errors), test_struct test_endian_table_init_subinterpreters.
  Needs real subinterpreter support (core / unassigned).
* test_importlib test_magic_number: expects exactly 3666 (CPython 3.15's own magic). Ours is
  3690..3699 chosen from a hash of the compiler sources so stale caches are never reused (the
  `+50` left over from the legacy/wordcode switch was removed, which fixes
  test_import.test_magic_number_endianness). Using 3666 would make us load CPython's own
  `cpython-315.pyc` files: coordinator decision.
* test_re DebugTests (4): re.DEBUG must print the parse tree and the compiled program in
  CPython's sre opcode format. Our engine (crates/sre) compiles to its own program, so this
  needs a dedicated dumper that reproduces sre's code generation; not attempted.
* test_audit test_cantrace: expects 4 traced hook calls; we now produce 2 (the hook is traced
  for the `exec` event of eval()). The test also counts a call for each assignment to
  `hook.__cantrace__`; I could not identify which audit event that assignment raises (it is
  not `object.__setattr__`, which audit-tests.py shows is raised only for class/`__class__`
  changes).
* test_capi (not in my list) still crashes, now later: test_tuple.test_tuple_getitem segfaults
  (tuple C API, core). The earlier crash in test_pep3118_obsolete_write_locks
  (`_io._BytesIOBuffer` with a NULL view / created by tp_alloc) is fixed.

## Decisions
* `_imp.create_dynamic` no longer goes through the Rust importer's `load_extension` (which put
  the module into sys.modules and ran the exec slots at once): it creates the module only, and
  `_imp.exec_dynamic` runs the pending multi-phase exec slots (capi_ext's implementation, now
  `pub`). The Rust importer's own path is unchanged.
* Extension suffixes: `.cpython-315t-x86_64-linux-gnu.so`, `.abi3t-x86_64-linux-gnu.so`,
  `.abi3t.so`, `.so` (no `.abi3.so`: the classic stable ABI assumes a GIL).
* warnings: dotted names derived from a file path are matched against a filter's module
  pattern only when the warning's module is unknown (warn_explicit without module); a warning
  from warn() is matched with its frame's `__name__` only.
* Launcher: without UTF-8 mode under an ASCII LC_CTYPE codeset, sys.argv is decoded as
  ASCII + surrogateescape (new `Config.argv_ascii`); the filesystem encoding itself stays
  UTF-8 (unchanged design).
* sqlite3 shell completion lives in `Lib/sqlite3/_completer.py` (own design: keywords from
  `_sqlite3.SQLITE_KEYWORDS`, functions from `PRAGMA function_list`, schema objects and columns
  from each attached schema's `sqlite_master` / `table_info`).
