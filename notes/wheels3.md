# Workstream "wheels3": cp315t wheels re-checked after refs1/capi3/scale/perf changes

Branch `worktree-agent-af7005cd67b17ebd3` (merged with `core` at c3dcde5: scale3, tail4,
cleanroom6, perf7). Reproducers: `tools/wheels/repro/*.py` (each prints OK; the ones that
build a C extension need `cc`; run them with a snapshot's or venv's binary).

## Measuring
* Snapshot: `tools/snapshot.sh $CARGO_TARGET_DIR/release/python3 . /tmp/w3/snap`; venvs
  from it with ensurepip; pip installs the PyPI wheels (and builds sdists: psutil, PyYAML).
* numpy: `tools/wheels/run_numpy_tests.sh -k -o DIR` (numpy 2.5.4, pytest 9.2.0,
  hypothesis 6.99.9), compared with the recorded baseline failure list.
* pandas 3.1.0: `run_numpy_tests.sh -k -o DIR -t pandas -T 9000 -m 6000000 -- -m "not slow
  and not network and not db and not single_cpu"`, compared with wheels2's baseline lists
  (failed ids, ids the baseline ran).
* Other packages: their own tests from the sdists (package directories renamed so that
  the installed wheel is imported), `pytest -o addopts=""`, same versions in a baseline
  venv on /opt/baseline/py-ft. Threads: pytest-run-parallel 0.10.0, `--parallel-threads=4`,
  on both interpreters; a test counts as ours-only only if it fails repeatably on ours and
  not on the baseline (tests that share files, seeds or global state fail on both, flakily).

## Results
| package | ours | CPython ft | failures only on ours |
|---|---|---|---|
| numpy 2.5.4 (full) | 50,575 passed, 4 failed, 41 errors | 50,574 passed, 5 failed, 41 errors | **0** (the 4th failure, test_big_arrays, is in the baseline list) |
| numpy test_multithreading | 34/34 | 34/34 | 0 |
| bitarray 3.12.3 | 740 passed | 740 passed | 0 |
| regex 2026.9.29 | 101 passed, 1 failed | same | 0 |
| markupsafe 3.0.4 | 79 passed | 79 passed | 0 |
| msgpack 1.2.4 | 160 passed | 160 passed | 0 |
| msgspec 0.22.0 (tests/unit) | 6,542 passed | 6,542 passed | 0 |
| pandas 3.1.0 (marker selection) | 182,560 passed, 3,344 failed, 1,362 errors (1:13 h) | 182,561 passed, 3,344 failed, 1,362 errors | **0** (same failure set as the baseline; was 16 before the fixes) |
| ujson 6.1.0 | 477 passed | 477 passed | 0 |
| simplejson 4.2.0 | 226 passed | 226 passed | 0 |
| charset-normalizer 3.5.2 | 300 passed | 300 passed | 0 |
| rpds-py 2026.9.1 | 132 passed | 132 passed | 0 |
| multidict 7.2.1 (with coverage, psutil) | 4,065 passed | 4,065 passed | 0 |
| propcache 0.5.4 | 57 passed | 57 passed | 0 |
| yarl 1.25.1 | 1,880 passed | 1,880 passed | 0 |
| wrapt 2.5.2 | 1,417 passed, 1 failed | same (the mypy-output test) | 0 |
| xxhash 4.0.2 | 276 passed, 1 failed | same (pyright test) | 0 |
| kiwisolver 1.5.1 | 50 passed | 50 passed | 0 |
| cramjam 2.14.0 | 583 passed | 583 passed | 0 |
| tornado 6.6 (`tornado.test.runtests`) | 1,501 run, OK | 1,501 run, OK | 0 (was 16) |
| cffi 2.1.1 (testing/) | 1,729 passed | 1,729 passed | 0 (was a segfault, then 2) |
| lxml 6.1.3 (src/lxml/tests under pytest) | 1,893 passed, 368 failed | 1,893 passed, 368 failed | 0 (see "lxml and the GIL") |
| pydantic-core 2.50.4 | 5,835 passed, 1 failed | 5,831 passed, 5 failed | 0 (was 6) |
| psutil 7.2.2 (sdist build) | builds and imports | builds | 0 (was a build failure) |
| greenlet 3.5.6 | does not import | — | not supported (see below) |

With `--parallel-threads=4` (ours vs baseline): bitarray, regex, markupsafe, msgpack,
msgspec, ujson, simplejson, charset-normalizer, rpds, yarl, propcache, kiwisolver, cramjam,
pydantic-core: no repeatable ours-only failure (the baseline itself crashed with SIGBUS in
bitarray's shared-file tests and SIGSEGV in xxhash's subinterpreter tests). multidict: 52
`test_finalizer_reentrancy` cases fail only on ours (list items, see "Left").

## Root causes fixed
1. **Fork-safe thread handles** (`_thread.rs`). A handle's state was a std Mutex; a thread
   finishing its run marks its handle done after leaving the stop-the-world protocol, so a
   fork in another thread could copy the mutex locked into the child, where threading's
   after-fork hook blocked on it (test_reinit_tls_after_fork, ~1 in 20). The state is now a
   futex word without a lock (joins wait on it, signals still interrupt the main thread),
   and `_thread`'s non-daemon list is locked across os.fork() (fork.rs) and emptied in the
   child. 40/40 test_reinit_tls_after_fork runs; repro fork_thread_handles.py hangs the
   old binary within ~8 rounds.
2. **STORE_SUBSCR keeps a counted reference** to a borrowed container of a heap type while
   its `__setitem__` runs (refs1 borrowed it): CPython counts a named object 3 times there,
   which getrefcount_borrow.py (wheels2) checks.
3. **Bound methods are unpacked by the interpreter's Call** before the call (the JIT tiers
   and CPython already do): pandas' NDFrame has `__getattr__`, so `df["a"].fillna(...,
   inplace=True)` loads a bound method that kept a second reference to the temporary and
   ChainedAssignmentError never came (15 pandas tests).
4. **LoadAttr instance-dict fast path read a dict entry without validation** (interpreter
   and tier 1): a concurrent `pop` could clear the entry between the key and value loads
   and the reader took a reference to NULL (propcache's concurrent eviction test,
   functools.cached_property). Now `dict::value_at_if_key` / `lookup_index_value` (sequence
   lock, null check) and a tier-1 helper while other threads exist.
5. **Heap-type references in builtin deallocs**: our builtin deallocs drop a heap
   subclass's type reference themselves; when an extension dealloc sits between (a heap
   type that calls the builtin's tp_dealloc and then Py_DECREF(Py_TYPE(self)), or a static
   type such as lxml's Cython exception classes, below which subtype_dealloc drops it) the
   reference was dropped twice: multidict's istr type freed after a few hundred instances
   (pickle segfault), lxml's SaxError freed twice at exit ("double free or corruption").
6. **Dict removals released at once** unless a lock-free reader may hold the pointer
   (table marked shared_read, or the writer is not the owner) — finalizers and weakref
   callbacks of removed values run at the removal as in CPython also in threaded programs.
   cleanroom6 made the same change on core; the merge keeps the owner check (a non-owner
   writer racing the owner's unmarked lock-free reads) and tail4's release_unpublished.
7. **bytearray growth with threads**: every replaced storage was kept until the next GC
   and growth never reallocated in place, so a growing bytearray held ~8x its size
   (tornado's decompression-bomb tests: tracemalloc peak 968 MB for a 100 MB body).
   Readers in other threads now mark the object (under the lock) and only then is old
   storage deferred; the owner otherwise reallocates/releases at once.
8. **re._parser.parse()** (re/_parse_tree.py, written from the documented tree shape) and
   the node codes in re._constants (hypothesis' `from_regex`: tornado property tests).
9. **zlib**: decompress(data, max_length) consumes an end-of-block code (and empty
   blocks) at the output limit like zlib, so a sync-flush marker is not left as
   unconsumed_tail (tornado websocket max_message_size).
10. **from-import from a non-module object** (cffi's `lib`) read a module dict out of it:
    segfault. **Extensions load with sys.getdlopenflags()** (cffi's RTLD_GLOBAL). Builtin
    call errors read `module.Type.name()` like CPython (also with a __self__); the builtin
    method repr names the self type by tp_name.
11. **pyexpat** repeats the error that stopped parsing on every later Parse() (ElementTree
    close() after a failed feed raises ParseError). ElementTree writes a URI registered with
    the empty prefix as the default namespace (pandas io/xml).
12. **Shutdown**: WeakValueDictionary's callback binds `_remove_dead_weakref` at definition
    (the end of every pandas run printed NoneType errors); after the modules are cleared
    the standard streams are set to None and the default unraisable hook is silent when
    sys.stderr is None, as in CPython (an asyncio loop set but never closed is finalized
    with the main thread's state; tornado's multiprocess test expects empty stderr).
13. **young buffers (scale3)**: compaction kept every duplicate of a reused address, so a
    thread allocating and freeing tracked objects between collections grew its buffer by
    one entry per allocation (the full pandas run aborted reserving 8 GB). Compaction drops
    duplicates when the stale-entry pass freed less than half (sort + dedup).
14. Smaller: sysconfig LDCXXSHARED/BASECFLAGS/CONFIGURE_*/BLDLIBRARY (psutil builds);
    pyconfig.h no longer defines what pyport.h/pythread.h define (warnings in every
    extension build); http.cookies quotes an empty value; `-i` on a pipe ends the prompt
    line at EOF; `_pydecimal` imported as itself has unbounded exponents (decimal keeps the
    accelerator's limits); PyBytesWriter always moves out of its small buffer (test_capi
    asserted it, flaky).

## lxml and the GIL
lxml's extension modules do not declare `Py_mod_gil = Py_MOD_GIL_NOT_USED`; CPython ft
re-enables the GIL when they load (RuntimeWarning). We have no GIL (DECISIONS: a
subinterpreter only reports one), so lxml's own threading tests
(test_concurrent_proxies, test_concurrent_class_lookup) are racy on ours ("Tried to
unregister unknown proxy"; they passed in the last run). A real fix needs a decision: a
global lock taken around calls into such modules, or a GIL mode.

## Not supported
* greenlet 3.5.6 (and gevent): needs `_PyGC_VisitFrameStack`, `_PyGC_VisitStackRef`,
  `_PyCriticalSection_SuspendAll/Resume` and switches CPython's internal thread-state
  frame/stack fields; our frames live in Rust structures.

## Left
* multidict's `test_finalizer_reentrancy` under pytest-run-parallel (52): items removed from
  a *list* are still released through QSBR while other threads run (list.rs `release_item`),
  so a removed key's `__del__` runs later than in CPython. Lists have no reader mark like
  dicts/bytearrays; the lock-free list readers (get_signed, tier-1/tier-2 inline subscript)
  would have to set one (qsbr::note_attr_read) for the owner to release at once.
* The full pandas run still aborts *after* its summary under `ulimit -v 6000000`
  ("memory allocation of 223053624 bytes failed"; memory growth over the 73-minute run).
* test_repl test_toplevel_contextvars_async fails on core too (not from this branch).

## Verification (after merging core, HEAD 85b6a2d)
* `tools/jit_tests/run.sh` and `tools/stress/run.sh`: all pass.
* `<snapshot>/bin/python3 -m test test_capi test_threading test_sys test_free_threading`:
  all 60 test files OK. Also run clean: test_decimal, test_re, test_zlib, test_gzip,
  test_zipfile, test_tarfile, test_pyexpat, test_xml_etree(_c), test_sax, test_minidom,
  test_http_cookies, test_call, test_repl (the one failure there is on core too),
  test_cmd_line, test_weakref, test_bytes, test_exceptions, test_descr, test_class.
* All `tools/wheels/repro/*.py` print OK.

## For the coordinator (proposals, not applied to shared files)
* D-level: extension modules without `Py_MOD_GIL_NOT_USED` (lxml) run without the GIL CPython
  ft would re-enable for them. Options: keep as is (documented), or serialize calls into
  such modules with a global lock.
* scale3's "no dedupe at compaction" is reverted in part (young.rs): dedupe only when the
  stale-entry pass frees less than half of the buffer, which is exactly the alloc/free churn
  case that grew without bound.
* Builtin deallocs and heap-type references (typeobject.rs `extension_drops_type_ref`): the
  rule is now "a builtin dealloc drops the type reference only when the chain from the
  instance's type reaches it through subtype_dealloc alone".
