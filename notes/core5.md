# core5 workstream notes

Branch `worktree-agent-a09599b93a591e888` (started from core4's branch `core` + master). Owner of the core
runtime (interp/ except jit + cache.rs + specialize.rs, types/ core, compiler crate, import system, sys*,
pymain, launcher, Lib/importlib, Lib/traceback.py, Lib/threading.py).

## Proposed decision for DECISIONS.md

### D25 — CPython's pyc magic number (pyc files are interchangeable with CPython 3.15's)
Finding: since D24 a code object's public form is CPython 3.15 wordcode, and marshal writes code objects in
CPython 3.15's layout ('c', five int32 counts/flags, co_code with zeroed caches, co_consts, co_names,
co_localsplusnames, co_localspluskinds, co_filename, co_name, co_qualname, int32 co_firstlineno, co_linetable,
co_exceptiontable). The pyc header (magic, flags, mtime+size or source hash) was already CPython's. The runtime
translator accepts every base 3.15 opcode (checked against spec/Include/opcode_ids.h; only pseudo/specialised/
INTERPRETER_EXIT are absent, and CPython never marshals those). So the formats are identical; only the magic
differed (we used 3690..3699, derived from a hash of the compiler sources so that a changed compiler invalidated
old caches).
Decision: `importlib.util.MAGIC_NUMBER` / `_imp.pyc_magic_number_token` / `PyImport_GetMagicNumber()` are
CPython 3.15's 3666 + b'\r\n' (test_importlib test_magic_number, test_capi test_getmagicnumber,
test_import test_magic_number_endianness). A .pyc we write is a CPython 3.15 .pyc and vice versa.
Invalidating our own stale caches: the header can no longer say "written by an older build of our compiler",
so the compiler crate's build script records `rpy_compiler::COMPILER_EPOCH` (the time the compiler/parser
sources last changed, i.e. when build.rs last re-ran), exposed as `_imp._rpy_compiler_epoch`;
`SourceFileLoader` treats a cache file whose file mtime is older than that as stale (recompiles and rewrites it).
One extra stat per cached import. The rule is ours only (CPython has no such check); consequences:
* CPython-written pycs older than our build are recompiled once (and rewritten in CPython's format, so CPython
  keeps loading them). In read-only directories they are recompiled in memory on every import.
* Sourceless .pyc files (SourcelessFileLoader) and zipimport are trusted as-is.
* `cp -r` resets mtimes, so a snapshot that copies a worktree's `Lib/**/__pycache__` would make stale caches
  look new. **Coordinator: tools/snapshot.sh / install.sh should not copy `__pycache__` directories**
  (e.g. `find "$DEST.tmp/lib" -name __pycache__ -prune -exec rm -rf {} +`).
Risk not verifiable here (rule 2 forbids running CPython): a pyc written by our compiler is executed by CPython's
VM if CPython finds it; bugs in our stack-depth/exception-table computation would surface there.

## Done

### pyc / marshal (item 1)
* D25 above. `marshal::CODE_FORMAT` removed; marshal module doc updated.
* Constant NaNs are never merged (compiler const keys, CFG `same_const`, runtime `ConstMerger`/shared const
  tuples): `{(1e999-1e999)*1j, (1e999-1e999)*2j, 1.5}` folds to a 3-element frozenset like CPython's
  (test_marshal test_deterministic_sets... the nan set test). test_marshal passes.

### test_descr (13 failures + 10 errors -> 2 failures, both cpython_only details)
* Tuple subclasses may have named `__slots__`: stored after the items (`descr::RPY_MEMBER_AFTER_ITEMS`, member
  offset `!position`, address via `descr::member_addr`, also used by clear_slots/subtype_traverse). Other
  variable-size bases (int, bytes, C types like HeapCCollection) accept only `__dict__`/`__weakref__` (pre-header);
  a class statement on a variable-size base no longer adds `__weakref__` implicitly; Py_TPFLAGS_ITEMS_AT_END is
  inherited.
* `__bases__` assignment layout check: GC-ness compared at the solid bases; managed dict/weakref the new base needs
  may come from the class itself.
* The shared `__dict__` descriptor defers to a builtin base's own `__dict__` descriptor (module dicts cannot be
  replaced, exception dicts not deleted).
* Slot recomputation (update_one_slot): a wrapper inherited from a builtin base for *another* slot with the same
  dunder (tuple.__add__ is sq_concat) gives this slot the base's value instead of the generic slot
  (NamedTuple + `__radd__`, gh146587).
* `__get__` of a descriptor class is called as found on the type (no binding: builtin `__get__`), test_bpo25750.
* RuntimeWarning for non-str keys in a class namespace; custom mro() leaving the class out of its own MRO makes
  super()/__init_subclass__ fail (in_mro check without identity shortcut); extending a class whose MRO is still
  being computed raises TypeError; dict assigned to `__dict__` released by tp_clear (self-cycles collected);
  staticmethod/classmethod `__annotations__`/`__annotate__` cache + AttributeError on missing delete;
  FileIO.closed doc.
* Left: test_vicious_descriptor_nonsense needs the jit5 fix in specialize.rs (notes/jit.md, "From core5"),
  test_method_get_meth_method_invalid_type and SharedKeyTests.test_subclasses (cpython_only internals).

### Lazy imports (test_lazy_import 13 -> 1)
* "Loaded" = in sys.modules and not initialising (`__spec__._initializing` read from plain storage, only the False
  object counts as done; no descriptors run).
* `lazy from m import a, b`: each name resolves on its own (module imported without the from-list, then the name
  looked up; a pending `m.a` entry of sys.lazy_modules is imported as a submodule), so using `a` does not import
  `b`'s submodule; errors from a package `__getattr__` become "cannot import name" with the cause.
* Module attribute fallback covers every pending entry; a lazily imported child deleted from its parent is found
  again; `finish` keeps entries whose module is still initialising.
* Re-entrant resolution of a placeholder while its import runs: from-imports see the (partial) module, plain
  imports the module they bind if it is fully imported, otherwise ImportError (gh-144727 case); a package's own
  `lazy from . import sub` read while resolving it counts as missing (no recursion).
* Shutdown clears sys.lazy_modules and the main thread's contextvars context (late `__del__` run);
  `_imp._set_lazy_attributes` exists (validates; attribute fallback makes storing unnecessary).
* Left: test_dunder_lazy_import_without_frame (the test pokes tstate->current_frame, which we do not maintain).

### test_cmd_line (24 -> 1), launcher
* -R, -X tlbc / PYTHON_TLBC validation, -X cpu_count / PYTHON_CPU_COUNT (`sys._get_cpu_count_config`, os.cpu_count),
  empty `-X pycache_prefix`, PYTHONDEBUG is a switch, warning options in precedence order (dev mode "default",
  PYTHONWARNINGS, -W, then -b/-bb as default::/error::BytesWarning; they are in sys.warnoptions as in CPython),
  closed std streams are not flushed at exit (no status 120), `-c` ignores coding cookies, sorted --help-env (own
  wording), colourised --help (PYTHON_COLORS/NO_COLOR/FORCE_COLOR/TERM/tty), -X importtime[=2] and
  PYTHONPROFILEIMPORTTIME (import.rs: timing around `_find_and_load`, "cached" lines), PYTHONMALLOCSTATS.
* Standard descriptors closed at process start: Rust's runtime reopens them on /dev/null before main, so an ELF
  constructor in the launcher records them (`RtConfig.closed_std_fds`); sys.stdin/stdout/stderr are None then.
* `_testinternalcapi.pymem_getallocatorsname()` reports "mimalloc" ("mimalloc_debug" with PYTHONMALLOC=debug or
  -X dev): our per-thread small-object allocator is in the place of CPython's free-threaded mimalloc.
* Left: test_dump_path_config (fatal path-config dump with a nonexistent PYTHONHOME).

### Rule 9 (item 3)
* Lib/importlib/util.py and Lib/threading.py rewritten from scratch without opening the old files (from the
  documented API and the spec tests). Same results: test_importlib 1236/0 (test_lazy_module_type_override now
  passes: a lazy module keeps a class its own code assigned), test_threading 247/4 (3 subinterpreter tests +
  test_no_refcycle_through_target), test_threading_local, test_thread, test_concurrent_futures and test_queue
  unchanged. threading uses `lazy import functools/warnings` (ensure_lazy_imports). importlib/_abc.py comment
  reworded.

### Other
* test_compile: a NUL byte on an earlier line is reported before an undecodable later line (parser tokenizer).
* test_generators passes: gi_state, close-failure message "Exception ignored while closing generator <repr>",
  pending signals are not handled at RESUME after yield from/await or at the backward jump to SEND
  (bpo-30039 behaviour; interpreter only — the JIT tiers poll at back edges, jit5 may want the same).
* Eval breaker after CALL (core4) kept, except right after a with statement's `__enter__` call (test_with
  InterruptDuringEnter; `_testinternalcapi.SelfInterruptingContextManager` is now a Rust type like CPython's C one).
* importlib: `_imp.create_dynamic` only creates multi-phase extension modules (exec_dynamic runs their slots;
  importlib registers them); extension suffixes are the free-threaded ones (no `.abi3.so`).
* threading: NaN lock timeouts raise ValueError; Condition.locked(); traceback: huge sys.tracebacklimit.

## Results (installed-layout snapshot, dev build)
test_descr 168/2, test_lazy_import 166/1, test_cmd_line 82/1, test_importlib 1236/0, test_import 128/5 (+11
subinterpreter errors), test_compile 188/0, test_marshal pass, test_generators pass, test_with pass,
test_threading 247/4, test_sys_settrace 448/0 (+1 error test_jump_from_yield), test_monitoring 100/5,
test_exceptions 115/4 (3 _testcapi no-memory tests, test_file_source caret width), test_warnings pass,
test_dis/test_peepholer pass, test_signal 57/4 (runs now; wakeup-fd error report has a traceback attached,
interrupt_main message — signal module).

## Open / for others
* jit5: specialize.rs load_attr_hit borrowed-descriptor issue (notes/jit.md); signal polling at yield-from back
  edges in tier 1/2; eval breaker after calls into C.
* stability2: test_no_refcycle_through_target (coordinator: fixed on their branch).
* Coordinator: D25 above; snapshot.sh should not copy `__pycache__`.
* Not done: test_monitoring's 5 (async for / match BRANCH events, TestLoadSuperAttr line ordering,
  test_disable_legal_events), test_jump_from_yield, test_exceptions' no-memory tests, supports_isolated_interpreters
  (no subinterpreters), test_sys SizeofTest.
