# PROGRESS

## Status 2026-11-06
All agents stopped 2026-11-02 when the account's weekly API limit was hit; resumed after the reset (Nov 5).

## RESUMED (2026-11-01, 22:15 UTC) after a pause at user request
Wave 10 running: jit3 (merge+enable tier 2, perf), numpy2 (numpy/other wheels own test suites vs baseline),
core3 (finalization, frozen modules, settrace, core failures), perf2 (startup, memory, scaling, shapes),
tk (_tkinter, tkinter, idlelib — D23). Measurement queue: suite run 5, then re-runs of both rebuilt baselines.

### Pause notes (2026-11-01, ~21:00 UTC)
In-flight agents were asked to wrap up; their branches are merged into master as they report.
To resume: read PLAN.md (workstream table, unassigned list), notes/*.md (per-workstream status and next
steps), DECISIONS.md D19-D22; integration happens in the /home/user/core worktree (branch `core`), pushed
to origin claude/rpython-dev. Latest full-suite run: run 4 (208/434 modules pass, 23,987 tests).
Known issue after the final merges: test_threading hangs (it failed without running tests in run 4); test_class has
2 failures from inline-values semantics (notes/perf-runtime.md). Leftover processes from finished agents may still
be running (killing them was not permitted in this environment).
PENDING MERGE: the jit2 workstream (tier-2 optimizing JIT, off by default; faster tier-1 Python->Python calls; type
profiles) is on remote branch `claude/jit2-tier2-unmerged` (notes/jit.md "jit2 workstream" on that branch). It conflicts
semantically with perf-runtime's frame changes already in master (borrowed code/func refs in Frame::new_call vs jit2's
owning alloc_call/recycle_call frame pool): port jit2's call path onto Frame::new_call, then merge and run
tools/jit_tests/run.sh. Tier 2 also has an open segfault in test_generators with RPYTHON_JIT2=1.
Not yet in spec: idlelib's own tests (Lib/idlelib/idle_test) are imported by test_idle but were not kept by the
D2 extraction — revisit D2 for test_idle.

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

## C ABI / wheels
**numpy 2.5.4 cp315t, full own test suite (numpy2, same wheels/pytest/hypothesis on ours and on py-ft):**
| package | ours | CPython ft | failures only on ours |
|---|---|---|---|
| numpy (full) | 50,575 passed, 3 failed, 41 errors | 50,574 passed, 5 failed, 41 errors | **0** |
| numpy test_multithreading | 34/34 | 34/34 | 0 |
| bitarray 3.12.3 | 740 passed | 740 passed | 0 |
| regex | 101 passed, 1 failed | same | 0 |
| markupsafe 3.0.4 | 79 passed | 79 passed | 0 |
| msgpack 1.2.4 | 160 passed | 160 passed | 0 |
| msgspec 0.22.0 | 6,541 passed, 1 failed | 6,542 passed | 1 |
| pandas 3.1.0 | crashes (use-after-free, stability2 investigating) | 182,561 passed | — |
numpy suite wall time: ours 7m30s vs CPython ft 10m12s. pip installs from PyPI over the network (ssl) and builds sdists.

### Earlier
pip (bundled 26.2.2 via ensurepip) installs the unmodified numpy-2.5.4-cp315-cp315t-manylinux wheel in a venv on
our interpreter; `import numpy` works (array math, linalg, fft, datetime64); numpy test_abc 5/5, test_api 59/59.
Blockers being fixed (numpy workstream): Cython code objects (numpy.random), C-created frame f_locals.

## Regression suite
Reference after the D21 rebuild (zstd + Tk; still no _decimal), ft: 414 PASS, 20 FAIL; tests run 51,220, failures 24,
skipped 3,276 (/opt/baseline/results/ft2). Default build (rebuilt): 414 PASS, 20 FAIL; run 51,147, failures 24, skipped 3,257.
Original Reference (CPython 3.15.2 free-threaded, this machine, tools/run_tests.sh):
434 modules: 416 PASS, 18 FAIL; tests run 50,649, failures 24, skipped 3,371.
(tools/baseline_ft_results.tsv). Default-build reference: 416 PASS, 18 FAIL; run 50,576, failures 24, skipped 3,353.

Ours (tools/run_tests.sh, 434 modules):
| run | rev | PASS | FAIL | CRASH | TIMEOUT | tests run | failures | skipped |
|---|---|---|---|---|---|---|---|---|
| 1 | 48f4a4f | 74 | 350 | 9 | 1 | 2,907 | 271 | 312 |
| 2 | 819c1f4 | 143 | 284 | 3 | 3 | 7,594 | 900 | 695 |
| 3 | d8d869f | 158 | 266 | 6 | 4 | 14,157 | 1,195 | 892 |
| 4 | 30f8658 | 208 | 215 | 6 | 4 | 23,987 | 1,165 | 1,632 |
| 5 | b578bf8 | 250 | 174 | 6 | 4 | 40,734 | 1,294 | 2,549 |
Run 3 top blockers: array (39 modules), http (21), urllib (12), multiprocessing (11),
importlib.resources (11), datetime (11), concurrent (9), xml/email (7 each), html (6).
Run 1 top blockers: py_compile (230 modules; merged since), unittest.mock (57), doctest (24),
array (24), socket (17), pathlib (16), http (14), multiprocessing, decimal, datetime, ntpath.

## Benchmarks
### Latest (jit4, 2026-11-07; steady-state callgrind instructions per loop, millions; ratio = ours / best CPython)
| bench | CPython default | CPython ft | ours | ratio |
|---|---|---|---|---|
| richards | 461 | 566 | 314 | 0.68 |
| nbody | 1108 | 1402 | 360 | 0.32 |
| spectral_norm | 1208 | 1404 | 636 | 0.53 |
| float | 778 | 870 | 702 | 0.90 |
| richards_super | 530 | 639 | 484 | 0.91 |
| chaos | 611 | 683 | 486 | 0.80 |
| coroutines | 294 | 300 | 290 | 0.98 |
| unpack_sequence | 0.25 | 0.15 | 0.11 | 0.73 |
| raytrace | 2757 | 3201 | 2776 | 1.01 |
| generators | 337 | 372 | 478 | 1.42 |
| fannkuch | 4393 | 4963 | 5780 | 1.32 |
| deltablue | 29.1 | 42.4 | 36.9 | 1.27 |
| hexiom | 58.1 | 65.7 | 82.7 | 1.42 |
| go | 1138 | 1260 | 1511-2167 | 1.3-1.9 |
| nqueens | 842 | 943 | 1860 | 2.21 |
| comprehensions | 0.19 | 0.24 | 0.39 | 2.1 |
Wall-clock pinned (s/loop, ours / default / ft): richards 0.0295/0.0299/0.0379, nbody 0.0261/0.0569/0.0784,
float 0.0460/0.0480/0.0566, chaos 0.0354/0.0376/0.0429, raytrace 0.197/0.188/0.207, generators 0.040/0.020/0.022,
coroutines 0.0160/0.0144/0.0141, nqueens 0.137/0.054/0.072, deltablue 0.00325/0.00221/0.00275, go 0.127/0.079/0.094.
Next (jit5): genexp regression after D24, generator resume/wall-clock, go deopt churn, temp tuples.

### Previous (jit3)
| bench | CPython default | CPython ft | ours | ratio |
|---|---|---|---|---|
| richards | 461 | 566 | 353 | 0.77 |
| nbody | 1108 | 1401 | 394 | 0.36 |
| spectral_norm | 1208 | 1404 | 818 | 0.68 |
| float | 778 | 870 | 866 | 1.11 |
| richards_super | 530 | 639 | 559 | 1.05 |
| chaos | 611 | 683 | 723 | 1.18 |
| raytrace | 2757 | 3201 | 3640 | 1.32 |
| hexiom | 58 | 66 | 78 | 1.35 |
| deltablue | 29 | 42 | 49 | 1.70 |
| go | 1138 | 1260 | 2111 | 1.85 |
| fannkuch | 4393 | 4963 | 8647 | 1.97 |
| coroutines | 294 | 300 | 605 | 2.06 |
| generators | 337 | 372 | 810 | 2.40 |
| nqueens | 842 | 943 | 2086 | 2.48 |
| comprehensions | 0.19 | 0.24 | 0.42 | 2.2 |
| unpack_sequence | 0.25 | 0.15 | 0.37 | 2.5 |
Next: generators/coroutines in tier 2 + deeper inlining (jit4); runtime costs in fannkuch/nqueens (perf2).

Startup and memory (perf2, quiet machine after reboot):
| metric | ours | py-default | py-ft |
|---|---|---|---|
| `-c pass` instructions | **35.0M** | 40.6M | 50.9M |
| `-c pass` wall, best of 30 | **13.4 ms** | 14.5 ms | 16.9 ms |
| `-c pass` peak RSS | 9.76 MB | **9.08 MB** | 10.64 MB |
| 200k small instances peak RSS | **35.2 MB** | 35.9 MB | 42.2 MB |
Scaling: not yet measured reliably (needs an idle machine and longer per-thread work); py-ft scaled
1/1.96/2.70/2.40 at 1-4 threads in a short quiet window.

### Earlier table (jit workstream)
Callgrind instructions, whole process, millions (machine heavily loaded, so instruction counts
are the primary metric; jit workstream, before generational GC / dict storage changes except float
and generators):
| benchmark | CPython default | CPython ft | ours | ours/default |
|---|---|---|---|---|
| richards | 527 | 654 | 627 | 1.19 |
| nbody | 1170 | 1452 | 1410 | 1.21 |
| unpack_sequence | 69 | 83 | 81 | 1.17 |
| float | 819 | 953 | 1108 | 1.35 |
| spectral_norm | 1251 | 1467 | 1831 | 1.46 |
| fannkuch | 4446 | 5020 | 6521 | 1.47 |
| hexiom | 133 | 155 | 194 | 1.45 |
| deltablue | 102 | 120 | 153 | 1.50 |
| chaos | 835 | 919 | 1332 | 1.60 |
| coroutines | 337 | 353 | 541 | 1.61 |
| nqueens | 886 | 991 | 1588 | 1.79 |
| go | 1201 | 1339 | 2204 | 1.84 |
| generators | 1029 | 1146 | 2174 | 2.11 |
Startup `-c pass`: 45M instructions (CPython default 40.5M); `-S -c pass`: 21M (CPython 27.7M).
Tiers: tier-0 inline caches + tier-1 x86-64 baseline JIT done; tier-2 optimizing JIT in progress (jit2).

### Earlier (preliminary, noisy machine)
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
