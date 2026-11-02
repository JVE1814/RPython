# RPython

> A from-scratch Rust re-implementation of CPython 3.15.2, with the free-threaded C ABI, no GIL, and a tiered JIT.

RPython aims to be a drop-in replacement for CPython **3.15.2** in its free-threaded build (`cp315t`).
Unmodified binary wheels such as numpy should load and run against it. The CPython regression
suite (`spec/Lib/test`) and the public C headers (`spec/Include`) are the specification. The
interpreter, compiler, builtin types, C API and stdlib are all written from scratch, without
reading CPython's or any other Python implementation's source.

The project is a work in progress. See [Status](#status) for current numbers.

## Table of Contents

- [Background](#background)
- [Install](#install)
- [Usage](#usage)
  - [Running the test suite](#running-the-test-suite)
  - [Benchmarks](#benchmarks)
  - [Environment variables](#environment-variables)
- [Status](#status)
- [Repository layout](#repository-layout)
- [Maintainers](#maintainers)
- [Contributing](#contributing)
- [License](#license)

## Background

Several constraints shape the design. [DECISIONS.md](DECISIONS.md) explains each one:

- **CPython's object layout.** C extensions use inline header functions that read object
  fields directly (`ob_tid`, `ob_ref_local`/`ob_ref_shared`, `ob_type`, tuple/list items, PEP 393
  strings, compact ints, type slots). So every runtime object has exactly CPython's
  free-threaded C layout, and refcounting uses the biased-refcount protocol the headers
  implement. There is no proxy layer, so calls between C and Python cost nothing extra (D4).
- **Memory management.** Reference counting, plus a stop-the-world generational cycle collector
  that coordinates threads with HotSpot-style safepoints. Objects never move (D5).
- **No GIL.** Each container takes its own per-object lock (`ob_mutex`) for mutation. Reads are
  lock-free and validated, and old storage is freed only after a safepoint. Racy programs must
  never crash (D6).
- **Tiered execution.** Tier 0 is an interpreter with inline caches and type profiles. Tier 1 is
  an x86-64 baseline template JIT with OSR. Tier 2 is a speculative optimizing JIT that
  deoptimizes back to the interpreter (D7). Code objects use CPython 3.15 wordcode as their
  public format (D24).
- **Rust everywhere.** The runtime, compiler and accelerator modules are Rust. Stdlib modules
  whose contract is Python source live in `Lib/` and are written from scratch (D8). The only
  dependencies are general-purpose crates. Big ints, regex, float formatting, compression,
  hashing and the JIT backend are all written in this repo (D9).

[PLAN.md](PLAN.md) describes the architecture and workstreams. [PROGRESS.md](PROGRESS.md) holds
test-suite runs and benchmark tables. `notes/*.md` has one status file per workstream.

## Install

Requirements: Linux on x86-64, and a recent stable Rust toolchain (`cargo`). A few optional
modules (`_ssl`, `_sqlite3`, `_ctypes`, `_tkinter`) load the system's OpenSSL, SQLite, libffi
and Tcl/Tk at runtime with `dlopen` (D20).

Build the interpreter:

```bash
cargo build --release -p python
```

This produces `target/release/python3`. Run straight from the build directory, it uses the
repo's `Lib/` as its stdlib.

To install a CPython-style prefix with `bin/python3.15t` (plus `python3`, `python3.15` and
`python` symlinks), `lib/python3.15t` and `include/python3.15t`:

```bash
tools/install.sh /opt/rpython
```

## Usage

The interpreter is used the same way as `python3`:

```bash
target/release/python3 -c 'import sys; print(sys.version)'
```

```bash
target/release/python3 script.py
```

Virtual environments and pip work. The bundled pip can install unmodified `cp315t` wheels:

```bash
/opt/rpython/bin/python3 -m venv venv
```

```bash
venv/bin/python -m pip install numpy
```

### Running the test suite

`tools/run_tests.sh` runs the regression suite one module at a time and writes
`<out-dir>/results.tsv`, with columns test, status, run, failures, skipped and seconds:

```bash
tools/run_tests.sh target/release/python3 /tmp/results
```

```bash
tools/run_tests.sh target/release/python3 /tmp/results test_list test_dict
```

`TEST_JOBS` sets the parallelism (default 2) and `TEST_TIMEOUT` sets the per-module timeout in
seconds (default 900).

The Rust crates have their own unit tests:

```bash
cargo test -p rpy-runtime
```

Differential JIT tests run each script under several tier/threshold configurations and compare
the output with the plain interpreter:

```bash
tools/jit_tests/run.sh target/release/python3
```

### Benchmarks

`tools/bench/` contains the benchmark harness: pyperformance benchmarks through a minimal
pyperf-compatible shim, plus startup, memory and multi-core scaling measurements. See
[tools/bench/README.md](tools/bench/README.md).

### Environment variables

Besides the standard `PYTHON*` variables, the runtime reads:

| Variable | Effect |
|---|---|
| `RPYTHON_JIT=0` | Disable the JIT (tier 0 only) |
| `RPYTHON_JIT_THRESHOLD` | Calls before a code object is compiled by tier 1 |
| `RPYTHON_JIT2=0` | Disable the tier-2 optimizing JIT |
| `RPYTHON_JIT2_THRESHOLD` | Hotness before tier 2 compiles a function |
| `RPYTHON_STDLIB` | Override the stdlib directory |
| `RPYTHON_PREFIX` | Override `sys.base_prefix` |

## Status

Results from the latest full regression-suite run (434 test modules), compared with CPython
3.15.2 free-threaded on the same machine:

| | modules passing | tests run |
|---|---|---|
| RPython (run 5) | 250 / 434 | 40,734 |
| CPython 3.15.2 ft | 416 / 434 | 50,649 |

What works today, in summary:

- Full 3.15 grammar parser and compiler. test_ast passes 217 of 220 tests; the tokenize, symtable and
  codeop suites pass.
- All 1,208 public C-API functions are exported. pip installs the unmodified numpy 2.5.4
  `cp315t` wheel, and `import numpy` works.
- Most of the stdlib: io/os, re, codecs (including CJK), unicodedata, json, pickle, datetime,
  decimal, asyncio, socket/ssl, http/urllib/email/xml, sqlite3, ctypes, multiprocessing,
  importlib.resources/metadata, venv/ensurepip, curses/readline/pyrepl, tkinter/idlelib.
- The tier-2 JIT is on by default. On richards, nbody and spectral_norm it runs fewer
  instructions than CPython. Generator-heavy code is still slower.

[PROGRESS.md](PROGRESS.md) has the per-run tables and benchmarks, and the "Unassigned" list in
[PLAN.md](PLAN.md) shows the known gaps (for example, subinterpreters).

## Repository layout

```
crates/parser    tokenizer + PEG parser -> AST, SyntaxError info
crates/compiler  symtable + AST -> bytecode, constant folding
crates/bigint    arbitrary-precision ints (PyLongObject digit layout)
crates/numfmt    float repr/format/parse, format-spec mini-language
crates/sre       regex engine with Python `re` semantics
crates/unicode   Unicode database tables
crates/compress  deflate, bzip2, LZMA/XZ, Zstandard, hash functions
crates/bindings  _ssl / _sqlite3 / _ctypes over system libraries
crates/runtime   libpython3.15t: objects, interpreter, GC, threads, C API, JIT, builtin modules
crates/launcher  the python3 executable
Lib/             pure-Python stdlib modules, written from scratch
spec/            pinned CPython tests and public headers (read-only)
tools/           bash/Rust tooling: test runner, benchmarks, install script
notes/           per-workstream status notes
```

## Maintainers

[@JVE1814](https://github.com/JVE1814)

## Contributing

Issues and pull requests are welcome. Before contributing, read [AGENTS.md](AGENTS.md). Its
clean-room rules apply to everyone, human or agent:

- Never read CPython's implementation source or any other Python implementation's source
  (PyPy, RustPython, ...). Use only the tests and headers in `spec/`.
- Never reproduce CPython code, comments or docstrings from memory.
- Never edit `spec/`, and never skip or special-case a test.
- Tooling is bash or Rust. Don't run a system Python.
- Add no external dependencies beyond general-purpose crates that contain no Python
  implementation code.

## License

MIT © RPython contributors (see `license` in [Cargo.toml](Cargo.toml)).

The contents of `spec/` are taken unchanged from CPython and remain under the Python Software
Foundation License; see [spec/LICENSE](spec/LICENSE).
