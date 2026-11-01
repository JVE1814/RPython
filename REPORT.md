# REPORT (interim — project paused at user request, 2026-11-01, day 1 of 14)

This is an interim report written at a pause, not the final report. The goal is **not** met.
PLAN.md (workstream/ownership table, unassigned work), PROGRESS.md (suite runs, benchmarks,
resume instructions), DECISIONS.md (D1-D22) and notes/*.md (one per workstream) hold the details.

## What exists
- Target pinned: CPython v3.15.2, free-threaded ABI cp315t. spec/ = Lib/test + Modules/_test* +
  public Include/ only (D2). Baselines: /opt/baseline/py-default and py-ft (PGO+LTO; rebuilt with
  zstd/Tk, still without _decimal because libmpdec is unavailable here — D21).
- Rust workspace (no CPython code read; recalled-CPython-text policy D19 with clean-room rewrites):
  parser (full 3.15 grammar), compiler (own bytecode), bigint, numfmt, sre, unicode (UCD 17),
  compress (deflate, bzip2, LZMA/XZ, Zstandard, MD5/SHA-1/2/3/BLAKE2 from scratch), bindings
  (_ssl/_sqlite3/_ctypes via dlopen of system libs — D20), runtime, launcher.
- Runtime: CPython-identical C object layout and C API (all 1,208 declared functions + 193 data
  symbols exported), biased refcounting, no GIL (per-object locks, stop-the-world safepoints,
  QSBR deferred freeing), TLAB allocator, generational cycle GC, tier-0 inline caches + tier-1
  x86-64 baseline JIT (own assembler, OSR, fallback to interpreter), inline instance attributes
  (shapes), lazy imports (PEP 810), sys.monitoring/settrace/setprofile.
- Stdlib: most of the pure-Python stdlib and its C accelerators re-implemented (io, os, re,
  codecs incl. CJK, unicodedata, json, pickle, datetime (C layout + capsule), decimal, array,
  struct, math, asyncio (+_asyncio), socket/select/ssl, http/urllib/email/xml (own expat-
  compatible parser, _elementtree), sqlite3, ctypes, multiprocessing/concurrent.futures,
  importlib.resources/metadata, venv/ensurepip (pip 26.2.2), curses/readline/_pyrepl, dbm, ...).

## Numbers (latest)
Regression suite (434 modules, tools/run_tests.sh):
| run | PASS | FAIL | CRASH | TIMEOUT | tests run | CPython ft reference |
|---|---|---|---|---|---|---|
| 4 (30f8658) | 208 | 215 | 6 | 4 | 23,987 | 416 PASS, 50,649 tests, 24 failures, 3,371 skips |
Many merges landed after run 4 (CJK codecs, email, asyncio/http, XML, stability, perf); they
are spot-checked only (e.g. test_email 1,818/1,818, test_codecencodings_jp, test_urlparse pass).

Performance (callgrind instructions; machine overloaded, wall-clock not meaningful — D22):
pyperformance 1.17x-2.11x CPython-default instructions (richards 1.19, nbody 1.21, go 1.84,
generators 2.11; table in PROGRESS.md). Startup `-c pass` 44.4M vs 40.5M (default build);
`-S -c pass` 20.7M vs 27.7M. Peak RSS `-c pass` ~15.7 MB vs 9.2 MB; 200k instances 50 MB vs 36 MB.
Multi-core scaling: not measured (needs an idle machine).

C ABI: pip installs the unmodified numpy-2.5.4 cp315t manylinux wheel; `import numpy` and
numpy.random work. numpy's own test suite has not yet been run on our interpreter.

## What does not work / open
- ~226 suite modules still fail/crash/time out; test_threading hangs; subinterpreters
  (_interpreters, concurrent.interpreters) absent; tkinter/_tkinter and idlelib absent; interpreter
  finalization (module teardown/__del__ at exit) missing; frozen modules stubbed.
- Tier-2 optimizing JIT (speculation/deopt/unboxing/inlining) in progress (jit2), not merged.
- Not yet faster than CPython on any pyperformance benchmark; startup and memory not beaten.
- numpy's own test suite and other wheels not yet validated.
- D2 gap: idlelib's own tests (needed by test_idle) were not kept in spec/.

## Process notes
- Incidents: several agents ran `python3 --version` once early on (D12); recalled CPython text
  found in some stdlib files and rewritten clean-room (D19).
- Integration: per-agent git worktrees, integration in /home/user/core (branch `core`), master
  pushed to origin `claude/rpython-dev`.
