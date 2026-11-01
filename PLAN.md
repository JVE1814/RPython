# PLAN

Target: drop-in Rust replacement for CPython **3.15.2** (free-threaded ABI, `cp315t`).
See DECISIONS.md for the rationale of each choice.

## Architecture
```
crates/parser    tokenizer + PEG parser -> AST (mirrors the `ast` module 1:1), SyntaxError info
crates/compiler  symtable + AST -> own stack bytecode (code units), constant folding
crates/bigint    arbitrary precision ints on 30-bit digits (layout == PyLongObject digits)
crates/numfmt    float repr/format/parse, int formatting, format-spec mini language
crates/sre       regex engine (Python `re` semantics) for str (UCS1/2/4) and bytes
crates/unicode   Unicode DB tables (unicodedata, str predicates/case mapping, normalization)
crates/runtime   libpython3.15t.so: C-layout objects, types, interpreter, GC, threads,
                 builtin modules, exported C API, JIT tiers
crates/launcher  python3.15t executable
Lib/             pure-Python stdlib modules written from scratch (see D8)
spec/            pinned CPython tests + public headers (read-only spec)
tools/           bash/Rust tooling: test runner, benchmark harness
```

## Phases
1. Foundation: parser, compiler, object model, interpreter, core builtins -> run simple scripts.
2. Bootstrap the test harness: enough stdlib (sys, os, io, re, functools, collections,
   unittest, test.support, libregrtest) to run `python -m test` on individual test files.
3. Breadth: grind test files by pass rate, module by module, in parallel subagents.
4. C API: export functions, build `_testcapi`/`_testlimitedcapi`/`_testbuffer` etc.,
   import binary extensions, pip install of numpy cp315t wheel.
5. Threads without GIL: per-object locks, safepoints, thread-safe refcounting.
6. JIT: tier-1 template JIT, tier-2 speculative JIT with deoptimization.
7. Performance: pyperformance, startup, memory, multicore scaling vs. baseline builds.

## Next steps
See PROGRESS.md.
