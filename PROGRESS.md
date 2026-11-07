# PROGRESS

## Status 2026-11-07 (day 7, afternoon)
Master 0a2f1fa: core6, perf4, tail1, capi2, cleanroom1+2 (20 Lib modules rewritten clean-room), scale1 (multi-core scaling), tail2 (re.DEBUG, _zoneinfo, ssl PSK, alloc-failure injection, 22 modules) merged today; suite run 13 running; suite run 12: 367/434 modules pass, 129 failures, 70 errors. Smoke on snap19: test_capi 40 failures (was 433 in run 11),
stable_abi_ctypes/fileutils pass, cmd_line_script 1; tail1 fixed ~60 long-tail modules (inspect, codecs, time,
path modules, math, buffer, logging, json, ...). Run 11 (pre-merges):
294/434 modules pass, 674 failures (241 without test_capi), 616 errors. Baseline free-threaded CPython: 416/434.
Running agents: scale2 (scaling round 2 + prompt-free regression in test_concurrent_futures), perf5 (remaining slow benchmarks), stability3 (test_free_threading races,
flaky crashes), tail3 (crossinterp, getpath, nturl2path, asyncio, ast, leftovers), pdb1 (clean-room pdb, remote pdb,
profiling.sampling).
Earlier history (2026-11-02): agents stopped when the weekly API limit was hit; resumed Nov 5.

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
| msgspec 0.22.0 | unit suite 6,373 passed, 0 failed (wheels2) | — | **0** |
| pandas 3.1.0 (baseline marker selection) | 182,557 passed, 3,347 failed, 1,362 errors (wheels2 run 2) | 182,561 passed, 3,344 failed, 1,362 errors | 3 at run 2 (xz presets, fixed since; that directory now passes) → expected **0** |
numpy suite wall time: ours 7m30s vs CPython ft 10m12s. pip installs from PyPI over the network (ssl) and builds sdists.
pandas: stability2 fixed the use-after-free (type refcount bias) and races; full suite runs in one process (peak RSS ~3.8 GB; a late "memory allocation failed" abort after the summary under a 6 GB ulimit remains). wheels2 fixed the behavioural differences (PyState_FindModule, zoneinfo transitions, datetime C API, 3.15 messages, getrefcount per D27, dict watchers).

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
| 6 | cd9478e | 259 | 158 | 13 | 4 | 39,273 | 660 | 2,377 |
| 7 | c8fda7a | 268 | 157 | 5 | 4 | 42,747 | 542 | 2,539 |
| 8 | 0b5f6a0 (jit5+core5, pre-lib6) | 280 | 144 | 3 | 7 | 42,339 | 275 | 2,485 |
| 9 | 433df5d (jit6+interp1, pre-stability2) | 288 | 135 | 6 | 5 | 42,388 | 224 | 2,486 |
| 10 | 55e6237 (stability2+perf3, pre-hang1) | 292 | 134 | 0 | 8 | 43,012 | 225 | 2,492 |
| 11 | 9af79cb (hang1, pre-wheels2/core6) | 294 | 140 | 0 | 0 | 49,781 | 674 (241 excl. test_capi) | 3,200 |
| 12 | f66b390 (core6+perf4+tail1+capi2) | 367 | 67 | 0 | 0 | 49,880 | 129 (+70 errors) | 3,202 |
Run 7 (pre-core4): crashes = test_capi (segfault after a lone-surrogate print), test_free_threading
(deque race, test_collections), test_itertools (flaky; passes now), test_multiprocessing_fork/spawn
(heap corruption in test_manager); timeouts = asyncio, cmd_line_script, os, subprocess. Largest
failure counts: sys_settrace 91 and monitoring 52 (both fixed by core4 since: 1 and 5), importlib 39,
cmd_line 28, call 17, inspect 15, lazy_import 13, descr 13, compile 12, traceback 10.
Run 8: crashes = test_free_threading (deque race), test_itertools (count race; fixed by lib6 since),
test_multiprocessing_fork; timeouts = asyncio, capi, cmd_line_script, multiprocessing_forkserver/spawn, os,
subprocess. Largest failure counts: importlib 20, call 17, inspect 15, dict 8, concurrent_futures 7.
Run 9: crashes = test_free_threading (io heap corruption), test_functools (lru_cache race; fixed since), test_super
(__class__ assignment race), test_multiprocessing_fork/forkserver/spawn; timeouts = asyncio, capi, cmd_line_script,
os, subprocess. Largest failure counts: importlib 20, call 17, inspect 15, dict 8, concurrent_futures 7.
Run 12: no crashes, no timeouts; 367/434 (baseline 416). 51 modules fail that pass on CPython: capi 40+13E, asyncio 10,
concurrent_futures 5, re 4, faulthandler 3, collections 3, multiprocessing_* 2 each, crossinterp 30E, sys 1+3E, gc 2, ast 2,
repl 2, ~30 modules with 1 failure; module-level errors: getpath (needs a getpath code object), nturl2path (missing),
pdb/remote_pdb (_pyrepl_available, _PdbServer), profiling (profiling.sampling missing), type_cache, tkinter/ttk (gui
resource: NO TESTS RAN exit status). Ours pass where the baseline fails: test_pydoc, test_site.
Run 11: no crashes, no timeouts. The jump in failures is test_capi (433), which timed out in run 10 and now finishes in 89 s
(capi2 is on it). Excluding it: 241 failures (225 in run 10; test_asyncio, cmd_line_script, multiprocessing_*, os were
also timeouts before). Top remaining: importlib 20, call 17, inspect 15 (both 0 after core6), asyncio 10, dict 8.
Errors (tests that raise; regrtest's total line omits them, run_tests.sh now records them in column 7): run 9 411,
run 10 484, run 11 616. Largest in run 11: builtin 163, capi 110, inspect 102, fileutils 35, stable_abi_ctypes 34, codecs 31,
crossinterp 30. Baseline (free-threaded CPython, tools/baseline_ft_results.tsv): 416 PASS / 18 FAIL, 24 failures.
Run 10: no crashes. Timeouts = asyncio, capi, cmd_line_script, multiprocessing_fork/forkserver/spawn, os, subprocess
(all fixed by hang1 since: each now finishes in 8–130 s). test_free_threading runs to completion (6 behavioural failures).
Run 3 top blockers: array (39 modules), http (21), urllib (12), multiprocessing (11),
importlib.resources (11), datetime (11), concurrent (9), xml/email (7 each), html (6).
Run 1 top blockers: py_compile (230 modules; merged since), unittest.mock (57), doctest (24),
array (24), socket (17), pathlib (16), http (14), multiprocessing, decimal, datetime, ntpath.

## Benchmarks
### Latest (perf5, 2026-11-07; wall ratio ours / best CPython, pinned one core, one run each, load 6–7, ±10–15%)
| benchmark | perf4 | perf5 |
|---|---|---|
| xml_etree | 1.55 | 1.55 |
| deepcopy | 1.52 | 1.36 |
| bpe_tokeniser | 1.27 | 1.30 |
| logging | 1.34 | 1.30 |
| python_startup | 1.33 | 1.29 |
| comprehensions | 1.35 | 1.27 |
| typing_runtime_protocols | 1.37 | 1.25 |
| mdp | 1.33 | 1.24 |
| base64 | 1.28 | 1.18 |
| async_generators | 1.25 | 1.17 |
| pathlib | 1.87 | 1.16 |
| nqueens | 1.30 | 1.14 |
| concurrent_imap | 1.00 | 1.08 |
| pprint | 1.11 | 1.02 |
| go | 1.03 | 0.97 |
| gc_traversal | 1.42 | 0.70 |
| gc_collect | 0.66 | 0.51 |
Instructions per call: bound method 447 (CPython 502), property read 401 (441), kw call 743 (701), raise+catch 4042
(3686). Peak memory 35.3 MB (CPython 32.4). Startup `-c pass` 55.8M instructions (40.5M). perf6 is on startup
(CDS-style startup image), slow call paths, xml_etree's expat engine, raise/catch, memory.

### Previous (perf4, 2026-11-07; wall-clock ratio ours / best CPython, pinned to one core, one run each, loaded machine, ±10%)
About 11 of 32 benchmarks beat or match the best CPython build (json_dumps 0.83, json_loads, raytrace, richards 0.60,
deltablue, chaos 0.65, float, hexiom, gc_collect, coroutines 0.98, unpack_sequence); fannkuch and go within a few %.

| benchmark | before perf4 | perf4 |
|---|---|---|
| pathlib | 2.12 | 1.76 |
| asyncio_tcp | 2.80 | 1.65 |
| concurrent_imap | — | 1.55 |
| deepcopy | 1.75 | 1.51 |
| mdp | — | 1.48 |
| gc_traversal | — | 1.46 |
| xml_etree | — | 1.45 |
| typing_runtime_protocols | 1.84 | 1.43 |
| logging | 1.67 | 1.35 |
| bpe_tokeniser | 1.65 | 1.31 |
| base64 | 2.11 | 1.27 |
| python_startup | 1.43 | 1.27 |
| pprint | 1.60 | 1.22 |
| async_generators | 1.51 | 1.20 |

Startup `-c pass`: 56.6M instructions (CPython 40.5M); `-S -c pass` 29.2M (27.7M). Peak RSS (startup_mem.sh):
34.9 MB (CPython default 32.3, free-threaded 38.6). Instructions per call shape and per loop: notes/perf4.md.
Multi-core scaling: being measured by scale1.

### Multi-core scaling (scale1, 2026-11-07; tools/bench/scaling, loaded machine — CPU inflation at 4 threads, ideal 1.00)
| workload | before scale1 | ours | py-ft |
|---|---|---|---|
| cpu_nbody | 1.48 | 1.08 | 1.07 |
| cpu_oo | 3.23 | 1.55 | 1.07 |
| cpu_float | 2.43 | 1.15 | 1.08 |
| cpu_comp | 3.09 | 1.07 | 1.15 |
| cpu_gen | 1.35 | 1.14 | 1.08 |
| shr_dict | 1.19 | 1.02 | 1.12 |
| shr_list | 1.36 | 0.95 | 1.33 |
| shr_attrs | 2.08 | 1.15 | 1.00 |
| alloc_cycles | 2.66 | 1.45 | 0.81 |
| mut_queue | 1.78 | 1.05 | 1.78 |
| mut_dict | 3.85 | 1.69 | 1.74 |
| mut_list | 3.42 | 1.70 | 1.52 |
| jit_calls | 9.16 | 2.60 | 1.03 |
| misc_classes | 3.49 | 5.22 | 2.43 |
| misc_cext | 1.42 | 1.08 | 1.05 |
Wall speedup at 4 threads beats py-ft on cpu_regex, shr_dict, shr_list (2.26 vs 0.89), alloc_dicts, mut_queue,
misc_import; behind on cpu_oo, jit_calls, shr_attrs, alloc_cycles, misc_classes, mut_list (scale2 is on these). Full
table and causes: notes/scale1.md. Quiet-machine run still due (D22).

### Previous (jit6 + perf3, 2026-11-07; wall-clock ratio ours / best CPython, targeted runs at load ~4, not quiet-machine)
| bench | jit5 | now | source |
|---|---|---|---|
| base64 | 2.98 | 1.45 | perf3 |
| json_dumps | 2.83 | 1.00–1.06 | perf3 |
| json_loads | 1.85 | 1.12–1.17 | perf3 |
| gc_traversal | 2.77 | 1.35 | perf3 |
| gc_collect | 1.49 | 0.81 | perf3 |
| meteor_contest | 1.83 | 0.87 | perf3 |
| sqlite_synth | 1.37 | 0.82 | perf3 |
| typing_runtime_protocols | 2.34 | 1.86 | perf3 |
| pathlib | 2.35 | 1.99 | perf3 |
| deepcopy | 1.86 | 1.54 | perf3 |
| xml_etree | 1.71 | 1.53 | perf3 |
| pprint | 1.66 | 1.39 | perf3 |
| mdp | 2.05 | 1.62–1.65 | jit6 + perf3 |
| generators | 2.05 | 0.94–1.11 | jit6 |
| async_generators | 2.04 | 1.34 | jit6 |
| comprehensions | 1.66 | 1.38 | jit6 |
| nqueens | 1.40 | 1.30 | jit6 |
| unpack_sequence | 1.49 | 0.99 | jit6 |
| go | 1.29 | 1.12 | jit6 |
| barnes_hut | 1.22 | 1.00 | jit6 |
| hexiom | 1.02 | 0.84 | jit6 |
| richards | 0.76 | 0.61 | jit6 |
Not yet worked on: asyncio_tcp (~2.5–3.3), bpe_tokeniser (~1.7), python_startup (~1.45), logging (~1.4).
Measured remaining causes: non-inlined Python call ~500 instructions (CPython 150–200) outside jit6's compiled
call routine; allocation ~100+50 instructions per object; expat allocates a Rust string per token. A full
quiet-machine pyperformance run (D22) is still due.

### Previous (jit5, 2026-11-07; steady-state callgrind instructions per loop, millions; ratio = ours / best CPython)
| bench | CPython best | jit4 | jit5 | ratio |
|---|---|---|---|---|
| richards | 461.1 | 314.3 | 282.8–322.6 | 0.61–0.70 |
| nbody | 1107.5 | 359.8 | 359.8 | 0.32 |
| spectral_norm | 1207.8 | 636.0 | 643.5 | 0.53 |
| float | 778.4 | 702.2 | 702.9 | 0.90 |
| richards_super | 530.4 | 484.4 | 454.0 | 0.86 |
| chaos | 610.7 | 486.3 | 408.0 | 0.67 |
| raytrace | 2757.1 | 2776.3 | 2704.8 | 0.98 |
| go | 1138.4 | 1511–2167 | 1173–1244 | 1.03–1.09 |
| generators | 337.1 | 477.9 | 351.8 | 1.04 |
| coroutines | 294.4 | 289.7 | 281.6 | 0.96 |
| nqueens | 841.8 | 1860.1 | 1043.7 | 1.24 |
| deltablue | 29.1 | 36.9 | 23.1–34.4 | 0.79–1.18 |
| hexiom | 58.1 | 82.7 | 53.8 | 0.93 |
| fannkuch | 4392.7 | 5779.9 | 3849.2 | 0.88 |
| unpack_sequence | 0.15 | 0.11 | 0.11 | 0.73 |
| comprehensions | 0.19 | 0.39 | 0.18–0.24 | 0.95–1.26 |

Full-suite wall-clock (jit5, s/loop, pinned to one core with other agents running; ratio = ours / best CPython).
Wins: chaos 0.78, deltablue 0.86, nbody 0.46, pidigits 0.54, regex_compile 0.17, regex_dna 0.57,
regex_effbot 0.51, regex_v8 0.88, richards 0.76, richards_super 0.92, raytrace 0.99, spectral_norm 0.53,
decimal_pi 0.03 / telco 0.06 (baseline lacks `_decimal`, D21).
Roughly even (1.00–1.10): coroutines, float, hexiom, pyflate, scimark.
Losses: base64 2.98, json_dumps 2.83, gc_traversal 2.77, asyncio_tcp 2.53, pathlib 2.35,
typing_runtime_protocols 2.34, generators 2.05, mdp 2.05, async_generators 2.04, deepcopy 1.86,
json_loads 1.85, meteor_contest 1.83, xml_etree 1.71, comprehensions 1.66, pprint 1.66, bpe_tokeniser 1.65,
gc_collect 1.49, unpack_sequence 1.49, python_startup 1.46, nqueens 1.40, logging 1.39, concurrent_imap 1.37,
sqlite_synth 1.37, go 1.29, barnes_hut 1.22, fannkuch 1.17. 26 benchmarks need CLI args or third-party
packages and did not run on any interpreter (list in notes/jit.md).
Wall-clock is much worse than instruction counts and load-sensitive (locked instructions, cache effects
valgrind does not model); `perf` is unavailable in the container.

### Previous (jit4, 2026-11-07; steady-state callgrind instructions per loop, millions; ratio = ours / best CPython)
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
