# PROGRESS

## Status (2026-11-01, day 1)
- Pinned CPython v3.15.2; spec extracted (`spec/`); baseline CPython 3.15.2 built twice
  (PGO+LTO default and free-threaded) in /opt/baseline.
- Finished foundation crates (each with its own test suite):
  - `rpy-unicode`: UCD 17.0.0 + 3.2.0; all 6 test_unicodedata checksums reproduced.
  - `rpy-numfmt`: float repr (Schubfach), correctly rounded formatting, format-spec mini
    language, %-formatting, parsing, hex; formatfloat_testcases.txt passes.
  - `rpy-bigint`: 30-bit-digit ints, Karatsuba/Toom-3/NTT, Burnikel-Ziegler, Montgomery,
    subquadratic str<->int; pidigits(10000) ~1.1 s.
  - `rpy-sre`: Python `re` engine; re_tests.py table + 106 ported test_re cases pass.
  - `rpy-parser` (in progress): all spec files parse; all test_ast snippets match; 815/815
    parser-level error messages; tokenize conformance 182/182.
- `rpy-compiler`: in progress (symtable, codegen, folding).
- `rpy-runtime`: first complete skeleton builds: C-layout objects with biased refcounting,
  type system with full dunder<->slot bridge, descriptors, builtin types core slots,
  exception hierarchy, interpreter loop (all opcodes of the contract), generators,
  frames/tracebacks, Rust import system, builtins, sys bootstrap. Initialization runs.

## Regression suite pass rate
Not yet runnable (waiting for the compiler).

## Benchmarks
Not yet measured.

## Next steps
1. Hook up the compiler; run first scripts; fix the VM.
2. Parallel workstreams (agents in worktrees): str methods; bytes/bytearray/memoryview;
   numbers+math; containers/iterators/weakref; stdlib bootstrap (os/posix, io, functools,
   collections, itertools, operator, re, types, warnings, traceback, unittest...);
   sys/threading/time/signal; C API + extension loading; cycle GC; JIT.
3. Test runner `tools/run_tests.sh` + pass-rate tracking.
