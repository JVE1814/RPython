# tail2 — long-tail test fixes (second round)

Measured on installed-layout snapshots (tools/snapshot.sh). Start: core 012738b (tail1 merged).
Quick loop: copy the new binary / changed Lib files into the snapshot; rebuild the snapshot
when tools/ or spec/Modules change.

## Done

### Allocation-failure injection (`_testcapi.set_nomemory`)
All checks go through `capi::mem::hooked_alloc_refuses` (probe the installed object-domain
allocator; returns at once while no C extension replaced an allocator) or the new
`capi::mem::hook_installed()` pre-check. Nothing on the TLAB fast path changed.
* exceptions constructed from Python (`BaseException.__new__`, so ExceptionGroup too) honour
  the hook; MemoryError and subclasses stay constructible (CPython keeps preallocated ones).
* subtype_dealloc: a dict materialised from inline values that outlives its object is
  "detached" in CPython (an allocation); a refusal is reported as unraisable MemoryError
  and the dict emptied (`typeobject::detach_shared_dict_hooked`). Our materialised dicts
  already own their values, so only the observable failure is reproduced.
* atexit: running the callbacks starts from a snapshot of the registry; a refused snapshot
  is reported and no callback runs.
* REPL (pymain, plain loop): reading/compiling a statement first probes the hook; after 17
  consecutive refusals the session exits with status 1 (CPython's endless-loop guard).
* pyexpat `ExternalEntityParserCreate`: parser object + engine state are two probes.
* BytesIO write (growing) and truncate (shrinking) probe the hook and leave the stream intact.

### re.DEBUG (crates/sre/src/debug.rs)
Independent of the matcher: the AST is reshaped to CPython's parse-tree form (non-capturing
groups spliced, common prefixes of alternatives hoisted, single-char alternatives turned into
a set, set members de-duplicated), dumped, translated into CPython's `_sre` code-word layout
(INFO block with literal prefix / charset, MARK, BRANCH/JUMP chains, REPEAT_ONE,
POSSESSIVE_REPEAT, ATOMIC_GROUP, GROUPREF_EXISTS, charsets compressed into literal runs,
CHARSET bitmaps or BIGCHARSET chunk tables, IGNORECASE variants) and listed. `_sre.compile`
writes the text to sys.stdout when flags contain DEBUG.

### `_zoneinfo` (crates/runtime/src/modules/_zoneinfo.rs)
Native ZoneInfo (subclass of the native tzinfo). TZif decoding uses
`zoneinfo._common.load_data` (as CPython's accelerator does); transition tables, the POSIX TZ
footer rules and all lookups are Rust. Base-class caches live in module state (weak cache:
a WeakValueDictionary; strong LRU of 8), subclasses get `_weak_cache` from
`__init_subclass__`; `_datetime` gained small pub helpers (flagged tail2).

### Smaller fixes
* settrace: a JUMP_BACKWARD that goes back within its own line reports a 'line' event
  (one-line loops/comprehensions; CPython's legacy-tracing JUMP callback). Not for
  JUMP_BACKWARD_NO_INTERRUPT (yield from), not for sys.monitoring.
* os.forkpty: the parent's master fd is non-inheritable.
* PyCF_IGNORE_COOKIE with invalid UTF-8 → UnicodeDecodeError.
* code.interact writes a newline when left through a local exit().
* `__lazy_import__` without globals honours a cleared `tstate->current_frame` (the C field
  now holds a non-null stand-in: the address of the base_frame slot).
* builtin_dict rare event: the builtins dict carries the JIT's watch tag from creation, so
  writes reach `cache::on_dict_write`, which counts them.
* Py_FatalError / _Py_FatalErrorFunc print through faulthandler's dumper (runtime state,
  Python stack, extension modules, C stack line).
* config_get('buffered_stdio') follows -u.
* ctypes: a pointer to an array has the PEP 3118 format "&(2,3)<h".
* _ssl TLS-PSK: set_psk_client_callback / set_psk_server_callback (identity hint),
  HAS_PSK and HAS_PSK_TLS13 true; client-only contexts set no session id context (the
  session OpenSSL makes for a TLS 1.3 PSK has none).
* _ssl: verify callback rejects certificates whose SubjectPublicKeyInfo SHA-256 is on a
  distrusted list (compromised eMudhra "EM OV TLS CA - G3C"), X509_V_ERR_CERT_REJECTED.
* Type versions: at most 1000 tags per heap type (then uncacheable, as CPython),
  PyUnstable_Type_AssignVersionTag really assigns, `_testinternalcapi.
  type_assign_specific_version_unsafe`, the adaptive (dis) recorder specialises attribute
  access / class calls / TO_BOOL_ALWAYS_TRUE only for types with a tag; ABC register of a
  collection flag invalidates the type caches.
* 1-char non-ASCII Latin-1 strings from `new_str` are the shared singletons.
* Docstrings that idle's test_calltip checks (types.MethodType, re.sub, Pattern.sub).

### Installed layout / tooling
* tools/snapshot.sh and tools/install.sh install include/python3.15t (spec headers +
  pyconfig.h) and bin/python3.15t-config (our shell script tools/python-config.sh, paths
  derived from its own location).
* tools/build_test_exts.sh also builds `_ctypes_test` (tools/ctypes_test) — test_ctypes ran
  327 of 624 tests without it.
* Lib/config-3.15t-x86_64-linux-gnu/Makefile: TESTSUBDIRS (generated from spec/Lib/test,
  non-empty dirs, plus idlelib/idle_test); sysconfig data: PY_BUILTIN_HASHLIB_HASHES.

## Findings
* test_tkinter / test_ttk: the baseline build has no `_tkinter` (test_tcl, test_idle,
  test_ttk_textonly also ran 0 tests there), so its run stops at `import_module('_tkinter')`
  = plain skip = exit 0. Ours has `_tkinter`, gets to `requires('gui')` = resource denied =
  NO TESTS RAN (exit 4). Under `xvfb-run -a python -m test -u gui test_tkinter test_ttk`
  both pass (1424 tests, 71 skipped). Harness/environment difference, not a bug.
* test_ctypes skips 4 recursion tests: `_testinternalcapi.get_c_recursion_remaining()` is
  large because our main thread runs on a big stack (design); pure Python recursion needs
  ~200 bytes of C stack per level here.

## Left (root causes)
* test_pdb / test_remote_pdb: Lib/pdb.py is a 1200-line skeleton; nearly every test_pdb
  doctest fails (146 doctests: 64 failures + 176 errors), and test_remote_pdb needs
  `_PdbServer`/`_PdbClient`/`_connect`. Adding `_pyrepl_available` alone makes the module
  load and then hang (restart loop on EOF: an empty script never stops, so the "program
  finished, restarting" loop spins). Needs a full clean-room pdb (own workstream).
* test_ast (2): non-str keyword keys are rejected at the call site for types and bound
  builtin methods; CPython 3.15 passes the dict to tp_call/VARARGS callees (ast types
  report "got an unexpected keyword argument <object ...>"). A general pass-through needs
  args::parse (and other kwnames readers) to tolerate non-str keys first.
* test_sys: per-interpreter intern table for dynamically allocated strings (ours is global
  and shapes key on interned pointers); SizeofTest.test_objecttypes (struct sizes).
* test_concurrent_futures (test_free_reference, test_no_stale_references) and
  test_multiprocessing_* test_release_task_refs: objects owned (biased refcount) by an idle
  pool worker that other threads release are queued for the owner, which never reaches its
  eval breaker while it waits. CPython merges such queues during the GC's stop-the-world.
  Tried here (drain every thread's queue while stopped): segfaults in test_thread_pool /
  test_free_threading, because our stop-the-world does not stop threads in native mode, so
  owners may still touch the counts. Reverted; needs a stop-the-world that covers every
  owner (stability3 / core).
* test_thread_local_bytecode (JIT-only threads record no specialisations), test_opcache
  test_125868 / test_dict test_splittable_popitem (no split tables), test_repl asyncio,
  test_descr / test_sys_settrace leftovers (see notes/core6.md).

## Rule-9
Lib/re/__init__.py: re.sub's docstring opening lines are the text test_calltip asserts (from
the spec); the rest is ours. Pattern.sub's first line likewise.
