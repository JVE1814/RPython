# pkgtools workstream notes

Branch: `worktree-agent-ae879d505a99ef9fa`.  All Python code here is written
from scratch (D19): structure, comments and docstrings are our own; only
test-observed names, messages and output formats match CPython.

## Delivered

| Area | Files | Spec status (this branch, release build) |
|---|---|---|
| importlib.resources | `Lib/importlib/resources/{__init__,_common,_adapters,_functional,_itertools,abc,readers,simple}.py`, `Lib/importlib/{readers,simple}.py` | all non-zip tests pass; zip tests need `zipimport` (compress2) |
| importlib.metadata | `Lib/importlib/metadata/{__init__,_adapters,_collections,_functools,_itertools,_meta,_text,diagnose}.py` | test_api + test_main all pass (needs `email`, now in master); test_zip needs `multiprocessing` |
| ensurepip | `Lib/ensurepip/{__init__,__main__,_uninstall}.py`, `_bundled/pip-26.2.2-py3-none-any.whl` (sha256 fd7f4dfa…3a70, from PyPI) | test_ensurepip 33/33 |
| venv | `Lib/venv/{__init__,__main__}.py`, `scripts/common/{activate,activate.fish,Activate.ps1}`, `scripts/posix/activate.csh` | test_venv 30 pass, 12 skipped (Windows/macOS, no csh, multiprocessing, ctypes for with_pip) |
| getpath / venv startup | `crates/runtime/src/modules/sys_full.rs` (`PathConfig`, `paths()`, `default_search_path()`, `import_site()`), `crates/runtime/src/lib.rs` | see "Runtime changes" |
| zipapp | `Lib/zipapp.py` | test_zipapp 36/36 |
| tracemalloc | `crates/runtime/src/modules/_tracemalloc.rs`, hooks in `alloc.rs` + `capi/mem.rs`, `Lib/tracemalloc.py`, launcher validation | test_tracemalloc 41 pass, 10 skipped (need `_testcapi`) |
| sysconfig | `Lib/sysconfig/{__init__,__main__}.py`, `Lib/_sysconfig.py`, `Lib/_osx_support.py`, `Lib/config-3.15t-x86_64-linux-gnu/Makefile`, `Lib/_sysconfig_vars_t_linux_x86_64-linux-gnu.json`, `SOABI_PLATFORM` in `_sysconfigdata` | 36 pass; test_get_config_h_filename fails in the dev layout only (passes with tools/install.sh, which installs include/pyconfig.h) |
| pydoc | `Lib/pydoc.py`, `Lib/pydoc_data/{__init__,topics,module_docs}.py` (own topic texts) | 105/119 pass (rest: runtime gaps below); test_enum's test_pydoc passes |

## Runtime changes (coordinate with core-runtime)

* `sys_full.rs`: CPython-like path configuration computed once (`paths()`):
  - `sys.executable` = argv[0] made absolute *without resolving symlinks*
    (searched on PATH when it has no '/'), so `venv/bin/python -> /usr/.../python3`
    finds the venv's `pyvenv.cfg`; falls back to `/proc/self/exe`.
  - `pyvenv.cfg` next to the executable or one level up => `sys.prefix =
    sys.exec_prefix =` that directory; `home` gives `sys._base_executable`
    and, via its real path, `sys.base_prefix`.  `PYTHONHOME` (unless -E) and
    `RPYTHON_PREFIX` override the base prefix.
  - `sys.path` after PYTHONPATH: `<base_prefix>/lib/python315t.zip` (always,
    like CPython), stdlib dir, `lib-dynload` if it exists.
* `lib.rs`: `site` is now imported *after* stdio/`builtins.open` exist
  (`site.venv()` reads pyvenv.cfg with `open`; before, it silently failed so
  venv site-packages were never added).
* `_tracemalloc`: `TRACING` atomic checked in `alloc_object`/`free_object`/
  `realloc_object` and the PyMem/PyObject domain functions; traces keyed by
  (domain, address), interned tracebacks of up to N frames (frames still in
  their MakeCell/CopyFreeVars prologue are skipped, like CPython's
  incomplete frames).  `PyTraceMalloc_Track/Untrack` moved here from
  `capi/mem.rs`.  PYTHONTRACEMALLOC / -X tracemalloc validated in the launcher.

## pip / numpy experiment (details in the final report)

Setup: `python3 -m venv /tmp/pk/venv1`, then `python -m ensurepip` and pip
installs from locally downloaded wheels (no `ssl` in this build, so pip
cannot reach PyPI itself).  Modules missing in master were borrowed for the
experiment only from other worktrees (optparse, http/urllib, zipimport,
uuid/tarfile, concurrent) plus throw-away stubs (http.cookiejar, xmlrpc,
csv) — none committed.

Result: pip 26.2.2 installs itself, `six`, `pytest` (+deps) and the
`numpy-2.5.4-cp315-cp315t-manylinux_2_27_x86_64` wheel; `import numpy`,
array math, linalg, fft, datetime64 and string arrays work.

Blockers found (owner in parentheses):
1. stdlib modules absent from master: optparse (markup), http.cookiejar (web),
   xmlrpc (nobody), uuid/tarfile/zipimport (compress2), ssl (bindings),
   concurrent.futures/multiprocessing (parallel), ctypes (bindings), zoneinfo (data).
2. `typing.NewType(...)(x)` raises "_idfunc() takes exactly one argument (2 given)":
   a builtin function stored as a class's `__call__` is called with `self`
   (slot_tp_call must not bind objects without `__get__`) (core-runtime).
3. `numpy.random` fails to import: Cython modules call
   `PyUnstable_Code_NewWithPosOnlyArgs` (empty code objects for tracebacks),
   which raises "CPython bytecode is not supported by this runtime" (capi).
4. Segfault in `frame.f_locals` for traceback frames of failing numpy tests
   (frames created by C extensions, e.g. Cython's PyFrame_New +
   PyTraceBack_Here) — crashes pytest on the first failing test (capi/core-runtime).
5. Builtin functions lack `__text_signature__` (e.g. `time.time`, `math.sin`,
   `os.stat`, `dict.__class_getitem__`), so pydoc/inspect show `(...)` (core-runtime).
6. `PYTHONIOENCODING` is not honoured for sys.stdout (osio/system).
7. `typing.List[int].__name__`, `(int | str).__module__` differ from CPython (lang).
8. A Protocol subclass with abstract methods can be instantiated (no
   TypeError) (lang/typing).
9. Our modules' `__builtins__` is the builtins module, not its dict (core-runtime).

## Notes for the coordinator

* tools/snapshot.sh does not install `include/python3.15t/pyconfig.h`
  (tools/install.sh does); with it test_sysconfig passes completely.
* `sysconfig` config vars BINDIR/LIBDEST/INCLUDEPY/LIBDIR now keep their
  build-time values (as CPython); location-dependent paths come from the
  install schemes.  The committed JSON vars file is derived from them.
* Test helpers used here: `tools/pkgtools_rt.sh` (regrtest) and
  `tools/pkgtools_ut.sh` (unittest; `DIR/` expands to its test modules;
  `EXTRA=` adds a PYTHONPATH entry).
