# PROGRESS

## Status (2026-11-01, day 1, evening)
- Spec pinned (CPython v3.15.2). Baselines built: /opt/baseline/py-default (PGO+LTO),
  /opt/baseline/py-ft (PGO+LTO, free-threaded).
- Foundation crates done & tested: unicode (UCD 17), numfmt, bigint, sre; parser (all spec
  files parse, all ast snippets/error messages match); compiler (compiles all 1269 parseable
  spec test files; all constructs).
- Runtime: first end-to-end Python execution (functions, classes, metaclasses, closures,
  generators, exceptions, comprehensions, f-strings, match). Own small-object allocator
  (TLAB-style), frame pool, stop-the-world cycle GC with safepoints.
- 12 parallel workstreams running in worktrees: str, bytes, numbers, containers, osio,
  functional, text, harness (unittest/traceback/...), system (sys/threads/importlib/...),
  lang (except*/typing/async gens), capi (C API + extension loading + numpy), jit.

## Regression suite
Reference (CPython 3.15.2 free-threaded, this machine, tools/run_tests.sh):
434 modules: 416 PASS, 18 FAIL; tests run 50,649, failures 24, skipped 3,371.
(tools/baseline_ft_results.tsv). Default-build reference: running.
Ours: regrtest not runnable yet (needs harness/system/osio/functional stdlib merges).

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
