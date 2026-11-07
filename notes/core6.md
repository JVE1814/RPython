# core6 workstream notes

Branch `worktree-agent-ae22c9eb299467b15` (from master 433df5d). Core semantics: object model, types,
compiler, eval loop, frames, tracing/monitoring, plus the small library pieces whose failures traced back
to them (inspect, pickle's frozendict rule, _datetime's public fields).

## Done

### Calls / type flags (test_call 17 -> 0)
* `tp_vectorcall_offset` is always inherited; `Py_TPFLAGS_HAVE_VECTORCALL` is inherited with an inherited
  `tp_call` (also by mutable heap types, unless the class body defines `__call__`); assigning `__call__`
  drops the flag on the class and on subclasses that do not define their own (`slots::update_slot`).
  `Py_TPFLAGS_METHOD_DESCRIPTOR` is inherited with `tp_descr_get` by immutable types only.
* `f(*t)`: the compiler passes the starred value to CALL_FUNCTION_EX as is (no BUILD_LIST/LIST_TO_TUPLE);
  a callable without vectorcall gets the exact tuple (interpreter and tier-1 helper `h_call_ex`, flagged
  "core6").
* Keyword-argument errors: `args::unexpected_keyword` (suggestion among keyword-capable names only,
  "invalid keyword argument for f()" for a str subclass equal to a parameter name, "keywords must be
  strings"), used by `args::parse`, print, min/max ("takes at most 2 keyword arguments"), itertools.product,
  ImportError. Python functions no longer suggest positional-only names. C functions without
  METH_KEYWORDS report `module.name() takes no keyword arguments`; slot wrappers name themselves.
* `dict.__contains__` is a METH_O|METH_COEXIST method (CPython's error messages).

### Pattern matching (test_patma 4 -> 0)
* frozendict matches itself with one positional sub-pattern; bytearray is not a sequence for `match`;
  a class without its own SEQUENCE/MAPPING flag takes it from the nearest MRO entry that has one.

### Functions (test_funcattrs 4 -> 0)
* Builtin static methods of types: `__self__` None, `__module__` from the type (tp_name prefix for static
  types — no attribute lookup, it runs during type setup).
* A function made with `FunctionType(code, globals)` whose globals lack `__builtins__` takes the running
  frame's builtins (not cached in NEWFUNC_CACHE).
* DeprecationWarning when `__code__` is assigned a code object of another kind (generator/coroutine/...).

### Dict / pickle / _testinternalcapi (test_dict 8 -> 2, test_opcache 7 -> 2)
* frozendict is not picklable with protocols 0/1 (generic reduce refuses it; _pickle and pickle.py).
* `_testinternalcapi.get_object_dict_values` (values in the class's key order: `shape::type_key_order`, the
  union of the class's shape keys in creation order; a dict materialised from inline values counts as
  sharing them, an assigned dict gives None), `dict_getitem_knownhash`, `compiler_cleandoc`
  (`rpy_compiler::clean_docstring`), `get_tlbc`/`get_tlbc_id`, `set_eval_frame_record/interp/default`,
  real rare-event counters.

### inspect (test_inspect 15 -> 0)
* Lib/inspect.py: `getfullargspec(annotation_format=)`, `Arguments` in `__all__`, partial is a method
  descriptor (but `_signature_is_builtin` excludes it), getclosurevars keeps attribute names in
  `unbound`, `__new__` keeps its binding (not unwrapped in `_signature_get_user_defined_method`), source of
  REPL-defined objects through `linecache._getlines_from_code`, BlockFinder ends a non-def statement at
  its NEWLINE (multi-line genexpr), CLI `--details` names targets after the defining module object.
* Compiler: class prologue names (`__module__`, `__qualname__`, `__firstlineno__`) go through nameop
  (a `nonlocal __firstlineno__` redirects the store, as in CPython).
* Generator/coroutine frames hold their free variables from creation (frame.f_locals before the first
  resume); `interp::copy_free_vars`.
* PYTHONPATH is read as bytes and decoded with surrogateescape.
* `_datetime`: timedelta.days/seconds/microseconds are member descriptors; `datetime.utcnow` is a builtin
  classmethod running the pure implementation. `os.login_tty` has a text signature.

### Monitoring (test_monitoring 5 -> 0)
* async for: the compiler emits NOT_TAKEN after the loop header's END_SEND (as CPython 3.14+); the
  interpreter fires BRANCH_LEFT when SEND completes into such an END_SEND (source SEND, destination the
  instruction after SEND, matching `co_branches()`), BRANCH_RIGHT at END_ASYNC_FOR (source SEND).
* match: the subject POP_TOP of a successful case is located at the next instruction (new `NEXT_POS`
  pseudo-location resolved by `cfg::resolve_next_positions`), so the branch into a case reports the body.
* LOAD_SUPER_ATTR is located at the attribute (like LOAD_ATTR), preceded by a NOP at the super() call
  (dropped when on the same line); its LINE event is deferred until after the super() CALL/C_RETURN events
  (`sys_trace::fire_deferred_line`).
* DISABLE returned for a branch event disables both directions of that instruction.

### PEP 523 test hook / rare events (test_optimizer 4 -> 1, test_capi Test_Pep523* 0 -> 11/12)
* While `set_eval_frame_record(list)` is active, every frame evaluation (including generator creation and
  resumes) appends the code name; the hook counts as "tracing" for `sys::TRACING`, so compiled code is not
  entered (no JIT changes needed).
* Counters: set_class, set_bases, set_eval_frame_func, func_modification (`__code__`, `__defaults__`,
  `__kwdefaults__`; kwdefaults assignment now also bumps func_version). Not counted: builtin_dict (needs
  dict watchers on the builtins dict).

### Thread-local bytecode view (test_thread_local_bytecode 6 -> 1)
* `interp::adaptive`: the main thread records specialisations into `CodeData.spec`, other threads into
  per-thread copies (`ThreadViews`, lock-free table of 63 slots; a thread's slot number is reused after it
  exits); `-X tlbc=0` / `PYTHON_TLBC=0` records nothing (no specialisation, as CPython). The eval loop
  computes the view once per activation.

### Annotations (test_type_annotations 3 -> 0)
* `__annotate__() must return a dict, not T`; implicit class cells (`__classdict__`, `__class__`,
  `__conditional_annotations__`) are always reached through the closure even if the class body binds the
  name.

### Later rounds
* test_sys 3 -> 1: `sys.implementation.supports_isolated_interpreters` is True (D26); struct sequences count
  their hidden fields in `__sizeof__`; `dict.__sizeof__` is computed in CPython keys-object terms (empty
  dict: object only; else 32-byte header + indices + usable entries at the size CPython would have grown
  to). Next blocker in SizeofTest.test_objecttypes: dict iterators (CPython-specific struct sizes, long tail).
* test_builtin 3 + 4 errors -> 0: eval/exec of top-level-await code returns a coroutine
  (`genobject::new_generator_named`, `interp::run_or_wrap`); exec closure validation; `input()` drops a
  failing `flush()` error and reports a lost sys.stdout; the compensated `sum()` (modules/builtins_numbers.rs)
  takes any int that converts to float; iterator `__reduce__` methods look `iter` up in the running frame's
  builtins (`builtins::eval_builtin`) before reading their state; PyCF_OPTIMIZED_AST folds `__debug__` at
  level 0; exec/eval compile warnings carry the globals' `__name__` as module.
* REPL (pymain): banner also with `-i` and piped stdin; a one-line compound statement waits for a blank line;
  EOF finishes a pending statement; sys.stdout/stderr flushed before each prompt (test_cmd_line_script's
  REPL tests no longer hang — they hung on master too); a post-parse SyntaxError gets the typed text.
* code.InteractiveConsole (Lib/code.py): any compile() failure is reported like a syntax error; a local
  `exit()` is not swallowed by runcode; sys.ps1/ps2 set only for the session.
* traceback: the nested-attribute hint skips modules (reading module attributes could resolve lazy imports).
* marshal keeps the co_consts tuple it read (constants shared between code objects survive a .pyc round trip;
  test_compile test_merge_constants failed on master from the second run on).
* compile errors: offsets of lines longer than 1000 bytes stay in bytes (CPython's line buffer).
* signal: wakeup-fd wording; `_thread.interrupt_main()` during finalization reports "Signal N ignored due
  to race condition" (test_signal 4 -> 0).
* -X importtime times imports of names with surrogates (also failing ones); a PYTHONHOME without a standard
  library stops startup with a path-configuration dump (test_cmd_line 2 -> 0).
* _ast: `ast.AST` attributes are replaceable (not an immutable type); an unknown non-str constructor keyword
  warns (DeprecationWarning) before the setattr fails. CALL_FUNCTION_EX leaves keyword-key checks to a
  non-type tp_call callee (it gets the dict as is); Python function binding rejects non-str keys itself.
* Failure injection (`_testcapi.set_nomemory`): `list.append` growth, `list()`, BUILD_LIST, `dict.copy()` and
  instances of Python classes (not exceptions) consult the hook (`capi::mem::hooked_alloc_refuses`):
  test_exceptions 4 -> 0, test_dict/test_opcache/test_list no-memory tests pass.

## Left (root causes)
* test_class test_detach_materialized_dict_no_memory, test_repl test_no_memory: other allocations (dict
  detach on dealloc, compiling at the prompt) do not consult the failure-injection hook.
* test_dict test_splittable_popitem: no split tables (getsizeof of an instance dict does not grow when
  popitem "combines" it).
* test_opcache test_125868: per-class shared-key capacity is not modelled (shapes are per insertion order).
* test_thread_local_bytecode test_threads_specialize_independently: a thread that only runs JIT-compiled
  code records no specialisations in its view.
* test_optimizer test_builtin_dict: needs dict watchers on the builtins dict.
* test_capi Test_Pep523AllowSpecialization.test_allow_specialization_call: CPython inlines Python-to-Python
  calls in the interpreter loop (no eval-frame hook call); every call of ours enters `eval_frame`.
* test_capi.test_misc TestPendingCalls.test_subthreads_can_handle_pending_calls hangs (also on master;
  hang1's area).
* test_sys_settrace test_jump_from_yield: an f_lineno assignment from the 'return' event of a yield must
  cancel the yield and continue at the target line (needs a jump path out of the yield exit).
* test_lazy_import test_dunder_lazy_import_without_frame: pokes tstate->current_frame.
* test_repl test_toplevel_contextvars_async: the asyncio REPL with piped stdin needs a one-line compound
  statement to be complete without a blank line (codeop/parser single-mode incompleteness rule).
* test_ast test_replace_non_str_kwarg / test_non_str_kwarg: bound builtin methods and classes get keyword
  keys checked at the call site (CPython 3.15 leaves it to callees).
* test_code_module test_exit (local exit message count).
* test_descr: test_vicious_descriptor_nonsense (jit5's specialize.rs note), cpython_only details.
* test_pdb does not import (`pdb._pyrepl_available` missing in Lib/pdb.py); adding it exposes hangs
  (test_dir_as_script, test_empty_file restart loops on EOF) — left to the pdb owner.
* test_opcache TestRacesDoNotCrash.test_for_iter_gen crashed once in ~10 runs ("double free or corruption")
  — race between generator iteration from two threads; stability2's area.

## Testing notes
* Snapshot: `bash tools/snapshot.sh $CARGO_TARGET_DIR/release/python3 <worktree> /tmp/core6snap`; for a
  runtime-only change copying the binary into /tmp/core6snap/bin is enough.
* Baseline comparisons used /tmp/interp1snap (master 8811764). regrtest's "Total tests" line counts
  failures only: compare the FAIL/ERROR lists.
