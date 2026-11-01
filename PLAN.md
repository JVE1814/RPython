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
Unassigned: _testinternalcapi/_testmultiphase/_testclinic re-implemented by us (spec lacks internal headers / generated clinic files); email/xml(pyexpat)/html/mailbox/mimetypes; pathlib/pwd/grp/
resource/termios/tty/pty/getpass/fcntl/mmap/marshal/py_compile/compileall/getopt/trace/pdb/
bdb/profile/cProfile/pstats/timeit/doctest; ctypes/_ctypes; multiprocessing/concurrent;
sqlite3 (own SQLite? decide); curses/readline/_pyrepl; no-GIL thread-safety audit.
