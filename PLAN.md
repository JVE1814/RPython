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

## Workstreams (branches `worktree-agent-<id>`, briefs follow AGENT_BRIEF.md)
Finished & merged: parser, compiler, str, bytes, numbers.
Running (wave 1): containers (list/tuple/dict/set/range/enumerate/zip/map/property/weakref/
frozendict), osio (posix/_io/time/os/io/stat/errno), functional (_functools/_collections/
itertools/_operator/_abc/functools/collections/enum/dataclasses/contextlib/types/copyreg-ish),
text (_sre/re/codecs/unicodedata/textwrap/string helpers/locale/difflib/shlex/glob/fnmatch),
harness (unittest/traceback/_warnings/argparse/logging/inspect/tempfile/shutil/test-support
deps), system (sys/threads/_thread/signal/gc/faulthandler/importlib/pymain/platform/sysconfig/
queue/tracemalloc), lang (except*/ExceptionGroup/asyncgen/typing/_typing/GenericAlias/
annotationlib), capi (exported C API, extension loading, _testcapi, numpy), jit (inline
caches/specialisation/JIT tiers).
Running (wave 2): ast (_ast/ast/tokenize/symtable/codeop), data (copy/pickle/json/datetime/
array/csv/calendar/pprint/string/zoneinfo/configparser/dbm), compress (zlib/bz2/lzma/zstd/
hashlib/hmac/base64/gzip/zipfile/tarfile/zipimport/uuid/secrets), net (_socket/select/
selectors/asyncio/_contextvars/subprocess?/urllib/http/ipaddress/socketserver).
Running (clean-room, D19): cleanroom-ut (unittest+mock/argparse/traceback), cleanroom-sys
(inspect/logging+config/tempfile/shutil), cleanroom-lang (enum/dataclasses/contextlib/_collections_abc).
Finished also: lang, functional, text, osio, containers, system, harness.
Running (wave 3): decimal (_decimal/_pydecimal), devtools (marshal/pathlib/pwd/grp/resource/termios/fcntl/mmap/dis/opcode/pdb/profile/doctest/timeit/runpy/pkgutil/site...).
Running (wave 4): compress2 (lzma/zstd/tarfile/zipimport/uuid; owns crates/compress), web
(ipaddress/urllib/http/wsgiref/_asyncio; owns asyncio + net modules), parallel
(concurrent.futures/multiprocessing/fork safety/threading), markup (pyexpat/xml/email/html/
mailbox/mimetypes/smtplib.../tomllib/optparse).
Finished also: capi (all 1208 C-API functions exported), compress (zlib/hashes/bz2/gzip/
zipfile), net (_socket/select/subprocess/asyncio).
Running (wave 5): bindings (_ssl/_sqlite3/_ctypes via system libs, D20), core-runtime (monitoring/settrace/gc/
 slots bugs, lazy imports, list reclamation, _testinternalcapi/_testmultiphase, core test failures).
Finished also: decimal (662/663 test_decimal), devtools.
Running (wave 6): pkgtools (importlib.resources/metadata, venv, ensurepip+pip, zipapp, pydoc, tracemalloc), intl
(CJK codecs, _multibytecodec, codec-name rules), markup2 (_elementtree, pyexpat perf, imaplib, xmlrpc, rule-9 review).
Finished also: data, markup (XML engine from scratch, email 1803/1814), cleanroom-ut, cleanroom-sys, cleanroom-lang.
Running (wave 7): jit2 (tier-2 optimizing JIT, deopt, tier-1 gaps), perf-runtime (inline attribute shapes,
call path, memory, safe reclamation D6, multi-core scaling, startup).
Finished also: jit (tier-0 caches, tier-1 baseline JIT, generational GC, startup 135M->45M).
Running (wave 8): stability (crashes, races, heap corruption, hangs; ASAN/TSAN).
Finished also: bindings (_ctypes/_sqlite3/_ssl via dlopen; crates/bindings), parallel (concurrent.futures,
multiprocessing, fork safety).
Running (wave 9): tty2 (_pyrepl, _tkinter+tkinter, login_tty, locking), numpy (C-API fixes until numpy's own test
suite passes; other cp315t wheels), docsig (__text_signature__ for all builtins).
Finished also: tty (dbm/readline/curses), pkgtools (importlib.resources/metadata, ensurepip+pip 26.2.2, venv,
zipapp, tracemalloc, sysconfig, pydoc). MILESTONE: pip installs numpy-2.5.4 cp315t wheel; import numpy works.
Running: core-runtime2 (finalization, except*, frozen modules, settrace/monitoring remainder, core test failures,
traceback hints).
Finished also: ast (test_ast 217/220, unparse/tokenize/symtable/codeop/grammar all pass), compress2 (lzma/zstd from
scratch, tarfile 779/781, zipimport, uuid), core-runtime (monitoring/setprofile, lazy imports PEP 810, gc policy,
_testinternalcapi, _testmultiphase, many slot/type fixes).
Running (wave 10, after resume): jit3, numpy2, core3, perf2, tk (see PROGRESS.md).
Wave 11 (after run 5): bc-compiler + bc-vm (D24: CPython 3.15 wordcode as public code format), grind-lib
(library-level suite failures). Finished: tk (_tkinter/tkinter/idlelib; test_idle 332 run/88 skipped = baseline).
Coordination file for D24: notes/bytecode.md.
D24 DONE (bc-compiler + bc-vm merged, master cd9478e): code objects hold CPython 3.15 wordcode, translated to the
internal form on first use; test_dis 145/145, test_code passes. Remaining: legacy pipeline removal; bare local-name
expression statement must stay a checked load (UnboundLocalError); settrace/monitoring event ordering.
Finished: jit3 (tier 2 merged + on by default; wins on richards/nbody/spectral_norm). Running: jit4 (generators in tier 2, inlining).
Later waves (2026-11-07): jit4, jit5, jit6 (callpy routine, generator chains, drain mode), core4/core5 (D24 follow-up, D25 pyc magic),
Wave of 2026-11-07 afternoon: core6 and perf4 merged; running capi2 (test_capi, stable_abi_ctypes, fileutils), scale1 (multi-core
scaling harness + fixes), tail1 (long-tail modules passing on CPython), perf5 (remaining slow benchmarks, in-JIT exception
handlers, startup), stability3 (test_free_threading env-changed races, test_stop_the_world, flaky crashes).
lib6 (stdlib failures), interp1 (subinterpreters, D26: done). Running: stability2 (free-threading crashes), perf3 (runtime fast paths).

Unassigned: _testclinic re-implemented by us (spec lacks
internal headers / generated clinic files); no-GIL thread-safety audit; startup time (jit owns perf).
