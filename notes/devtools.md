# devtools workstream notes

## Modules
Rust (crates/runtime/src/modules/, registered from `devtools.rs`):
marshal, pwd, grp (pwd.rs), resource, fcntl, termios, syslog + _posixshmem (syslog.rs), mmap,
_lsprof (lsprof.rs); _opcode.rs (taken over from harness).
Python (Lib/): getopt, colorsys, graphlib, sched, this, antigravity, timeit, trace,
ntpath, pathlib/ (package), py_compile, compileall, filecmp, doctest, pdb, bdb, cmd, code
(+ private _codeop_fallback), getpass, tty, pty, runpy, pkgutil, site, dis, opcode,
pstats, profile, cProfile, profiling/ (+ profiling.tracing), netrc, tabnanny, wave,
webbrowser, modulefinder, pyclbr.

## Decisions
### marshal: code objects
Our code objects are serialised field by field (TYPE_CODE): argcount, posonlyargcount,
kwonlyargcount, stacksize, flags, co_code (bytes of 32-bit LE words), consts, names,
localsplusnames, localspluskinds, filename, name, qualname, firstlineno, linetable
(16 bytes per instruction), exceptiontable (20 bytes per entry).  Loading validates the
sizes ("bad marshal data (invalid code object)").  Version 6 is written by default.
`marshal._loads_without_positions(data)` (private) drops column information (used by
doctest for synthetic traceback frames).

### .pyc files
importlib's standard 16-byte header (magic 3999 from importlib) followed by marshal of
our code objects.  py_compile/compileall/runpy follow the CPython file layout
(__pycache__/name.rpython-315.pyc via importlib.util.cache_from_source).

### pathlib
Own design: pathlib/_os.py helpers (vfspath/vfsopen/copy), pathlib/types.py protocols,
pathlib/_glob.py segment translator.  Path.info, copy/move implemented.

### doctest / pdb / bdb
doctest runs examples through its own `_CollectingRunner`; DocTestCase uses subTest and
synthetic traceback frames.  pdb.set_trace() stops inline (the `_paused` flag makes the
dispatcher ignore events until interaction ends).

### profiling (cProfile)
`_lsprof.Profiler` (Rust) registers as sys.monitoring tool 2 ("cProfile") and handles
PY_START/PY_RESUME/PY_THROW, PY_RETURN/PY_YIELD/PY_UNWIND, CALL and C_RETURN/C_RAISE.
Entries keyed by code object (or PyMethodDef pointer for builtins), one call stack per
thread, mutex never held while the external timer (Python code) runs; the timer may not
clear/disable/re-init the profiler (RuntimeError -> unraisable).  `profiling.tracing` is
the Python layer; `cProfile` re-exports it.  `profile` (pure Python, deprecated) uses
sys.setprofile.  `profiling.sampling` is NOT implemented.

### site (PEP 829)
Own design around `StartupState` (pending path entries, .pth import lines, .start entry
points; process() applies paths, then imports, then entry points).  `addsitedir(dir, known)`
skips directories already in a supplied `known` set (idempotent); main() batches venv,
user and global site dirs in one state.

### dis
Clean-room rewrite for our bytecode (32-bit words, absolute jumps, no caches): decoding
with EXTENDED_ARG start offsets, ArgResolver with per-opcode describers, Formatter,
Bytecode, _find_imports/_find_store_names.  `deoptmap` is empty.
`_opcode.stack_effect` now accepts an oparg for every opcode (missing = 0).

### modulefinder / pyclbr
modulefinder scans compiled code with dis (works for source and .pyc); pyclbr uses ast.

### D19 clean-room
All Python files listed above are my own structure/wording; dis.py, site.py and opcode.py
(inherited from other workstreams) were rewritten from scratch after taking ownership.

## Cross-workstream issues found
- int::as_i128 (types/int.rs) loses high bits for 5-digit values (2**128 -> 0). Worked around
  with `devtools::int_as_i128` (rejects > 4 digits).
- pymain: `-m` ran runpy without creating `__main__` (fixed, one line in pymain.rs).
- genericpath.ALLOW_MISSING.__reduce__ returns the class name instead of 'ALLOW_MISSING'
  (pickle test in test_genericpath/test_ntpath fails) — osio.
- sys.settrace: the audit hook runs (and is traced) after the trace function is installed;
  line events: `if []:` reports 2 hits, generator lines off by one (system/core).
- Audit hooks are traced/profiled (CPython skips hooks unless they set __cantrace__):
  shows up as `_test_audit_hook` in profile output (sys_full::audit).
- sys.monitoring never fires CALL / C_RETURN / C_RAISE / PY_THROW, and sys.setprofile
  never reports c_call / c_return / c_exception: cProfile/profile cannot see builtin
  calls (test_profile / test_tracing_profiler expected output needs them).
- slice objects: `==` not implemented (containers).
- compile(): "unknown encoding: U7" (utf-7 coding cookie) — text/compiler.
- os imports warnings eagerly (test_site.test_lazy_imports).
- site is imported at start-up (sys_full::post_init) before sys.stderr is usable: output of
  .pth import lines run during start-up is lost (test_site *_already_on_path tests).
- Missing modules blocking spec tests: unittest.mock, array, urllib, multiprocessing,
  _testmultiphase (test_importlib.util), optparse.
- pickle has no _pickle accelerator, so pickle.Pickler is the Python class
  (test_pyclbr.test_others expects it to come from elsewhere).
