# PROGRESS

## Status (2026-11-01, day 1, night)
- Spec pinned (CPython v3.15.2). Baselines built: /opt/baseline/py-default (PGO+LTO),
  /opt/baseline/py-ft (PGO+LTO, free-threaded).
- Foundation crates done & tested: unicode (UCD 17), numfmt, bigint, sre; parser (all spec
  files parse, all ast snippets/error messages match); compiler (compiles all 1269 parseable
  spec test files; all constructs).
- Runtime: first end-to-end Python execution (functions, classes, metaclasses, closures,
  generators, exceptions, comprehensions, f-strings, match). Own small-object allocator
  (TLAB-style), frame pool, stop-the-world cycle GC with safepoints.
- Finished workstreams merged to master: parser, str (full str type, two-way search, format/%),
  bytes (bytes/bytearray/memoryview/buffer protocol), numbers (float/complex/int/math/cmath/
  _struct/binascii/_random/random/fractions/statistics).
- Running: compiler, containers, osio, functional, text, harness, system, lang, capi, jit;
  wave 2: ast (_ast/ast/tokenize/symtable), data (pickle/json/datetime/array/copy/csv...),
  compress (zlib/bz2/lzma/zstd/hashes/zip/tar), net (socket/select/asyncio/urllib/http).
- Known blockers for importing os/functools/collections: range() with big ints (containers),
  warnings.warn, sys.excepthook (harness), enum (functional).

## Regression suite
Reference (CPython 3.15.2 free-threaded, this machine, tools/run_tests.sh):
434 modules: 416 PASS, 18 FAIL; tests run 50,649, failures 24, skipped 3,371.
(tools/baseline_ft_results.tsv). Default-build reference: 416 PASS, 18 FAIL; run 50,576, failures 24, skipped 3,353.

Ours (tools/run_tests.sh, 434 modules):
| run | rev | PASS | FAIL | CRASH | TIMEOUT | tests run | failures | skipped |
|---|---|---|---|---|---|---|---|---|
| 1 | 48f4a4f | 74 | 350 | 9 | 1 | 2,907 | 271 | 312 |
| 2 | 819c1f4 | 143 | 284 | 3 | 3 | 7,594 | 900 | 695 |
Run 1 top blockers: py_compile (230 modules; merged since), unittest.mock (57), doctest (24),
array (24), socket (17), pathlib (16), http (14), multiprocessing, decimal, datetime, ntpath.

## Benchmarks (preliminary, noisy machine)
| metric | ours | py-default | py-ft |
|---|---|---|---|
| startup `-c pass` | 6.6 ms (no site yet) | 13.6 ms | 19.4 ms |
| fib(20) instructions (callgrind) | 42.4M (35.8M excl. startup) | 51.1M (11.5M excl. startup) | - |
pyperformance: harness ready (tools/bench), ours blocked on stdlib (time/argparse).

## Next steps
1. Merge workstreams as they complete; get regrtest running; first pass-rate measurement.
2. Wave 2 stdlib (asyncio, pickle, json, datetime, decimal, socket/ssl, hashlib, zlib,
   sqlite3, email/http/urllib/xml, multiprocessing, ctypes, csv, zipfile/tarfile...).
3. Performance: inline caches, baseline JIT, optimizing JIT (jit workstream).
