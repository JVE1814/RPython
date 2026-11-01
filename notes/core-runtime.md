# core-runtime workstream notes

Owner of crates/runtime core (interp except jit/specialize/cache, core types, gc.rs,
object/alloc/errors/abstract_/args/methods/compile/builtins/import, sys*, pymain, launcher).

## Done in this round
* **PEP 810 lazy imports** (`types/lazyimport.rs`): `lazy_import` type (`types.LazyImportType`
  via `sys._lazy_import_type`), `builtins.__lazy_import__`, `sys.lazy_modules` (set),
  filter / "all" mode / `__lazy_modules__`, reification in LOAD_GLOBAL/LOAD_NAME/
  LOAD_FROM_DICT_OR_GLOBALS/module getattr/`resolve()`, attribute placeholders for
  `lazy from` and `lazy import a.b as c` (attribute names replayed, flat — no deep
  recursion), pending-submodule fallback in module getattr, notes on failed reification.
  Compiler: ImportName arg bit 23 = `lazy` keyword, bit 24 = "may become lazy implicitly"
  (module level, outside try/except handlers). dis.rs/Lib/dis.py read bit 23 only.
  -X lazy_imports / PYTHON_LAZY_IMPORTS accept only all|normal. C API mode/filter share
  sys's state. **For jit**: `specialize.rs` must not cache lazy placeholders found in module
  dicts (done for the MODULE kind); `eval::load_global` (the cache-fill path) reifies.
* **sys.monitoring**: CALL, C_RETURN/C_RAISE (enabled by CALL), JUMP, BRANCH_LEFT/RIGHT,
  STOP_ITERATION (exhausted generator in FOR_ITER), RERAISE, EXCEPTION_HANDLED, PY_THROW;
  LINE only at line starts; get_events/get_local_events on unused tools return 0;
  callbacks of line/jump/branch events may set f_lineno.
* **settrace/setprofile**: c_call/c_return/c_exception (bound methods unpacked; method
  descriptors reported bound), line events on forward jumps to the same line suppressed,
  a raising tracer no longer bypasses cleanup handlers (stale handled exceptions leaked).
  `frame.f_lineno = n` (interp/jump.rs): stack-kind analysis of our bytecode, CPython's
  error messages, popping saved exc_info restores the handled exception, unbound locals
  read on the target path become None with the RuntimeWarning.
  Audit hooks run untraced unless they set `__cantrace__`.
* **Startup**: std streams are created before `site` (so .pth output works and `open`
  exists); `<argv0>._pth` files (fixed sys.path, no PYTHONPATH/stdlib, site only with
  `import site`, implies -P and no user site); AppleFrameworkLoader in importlib.
* **pyc magic** derives from a hash of the compiler+parser sources (crates/compiler/build.rs,
  `rpy_compiler::BYTECODE_HASH`), 3690..3699; `_bootstrap_external.MAGIC_NUMBER` reads
  `_imp.pyc_magic_number_token`. Stale caches from older compilers are no longer reused.
* **GC**: weakrefs with callbacks cleared before finalizers, others before tp_clear;
  policy pieces in `gc_support.rs` (threshold 0 disables, stats incl. candidates/duration,
  per-generation stats for explicit gc.collect(0/1), DEBUG_SAVEALL, instance dicts not
  counted as collected objects); get_referents no longer requires tracked objects;
  PyUnstable_GC_VisitObjects walks `gc::all_tracked()`.
* **Types**: generic `__hash__` slot uses an int result that fits as is; static types without
  tp_new deriving from object are DISALLOW_INSTANTIATION; static types are IMMUTABLETYPE;
  slot_tp_init/slot_tp_call bind non-function descriptors via `__get__`; richcompare treats
  an AttributeError from `__get__` as NotImplemented; `__del__` changing `__class__`;
  `__bases__` cycle check through tp_base; subclasses' slots fixed after `__bases__`
  assignment; heap type `__name__`/`__qualname__` return the stored objects; shared
  `__dict__`/`__weakref__` descriptors owned by object; `__classcell__` set before mro();
  `__classdictcell__` holds the class namespace; super: descriptor binding, members,
  repr, traverse, re-init; Py_TPFLAGS_INLINE_VALUES + `has_inline_values` model.
* **Misc**: int.as_i128 overflow; generator throw of a class whose call returns a
  non-exception; throw into delegating generators links the outer frame; generator
  lambdas return their value (compiler); `global` declarations hide enclosing bindings
  from nested scopes (symtable); `x is (1, 2)` SyntaxWarning; with-statement messages and
  sync/async hints; exception context chain cycles (Floyd); recursion guard in
  isinstance/issubclass tuples and `__bases__` walks; thread stack margins scale with small
  `threading.stack_size()` stacks; list subscript fast paths read under the list lock
  (eval + jit helpers); builtin module cache holds weak references (collectable modules);
  `_suggestions` module; `-X no_debug_ranges`; PEP 741 config names.
* **Test helpers**: `_testinternalcapi` (Rust builtin, modules/testinternalcapi.rs: inline
  values, recursion/stack, iframe_*, SelfInterruptingContextManager, constants, ...);
  `_testmultiphase` built by tools/build_test_exts.sh using the hand-written
  tools/capi_shim/clinic/_testmultiphase.c.h; PEP 793 Py_mod_create gets def=NULL.

## Measured (dev binary, PYTHONPATH=spec/Lib + test extensions)
test_class 42/42, test_with 55/55, test_sys_setprofile 29/29, test_scope, test_super,
test_grammar now run clean or with 1 failure; test_descr runs (was a crash); test_import
runs (was skipped: 127 tests, 22 failures); test_lazy_import 111/166 (was a timeout; many
of the remaining failures are `-I` subprocess tests that cannot see spec/Lib in a dev run);
test_sys_settrace 262 -> fewer failures (jump support added after the last measurement);
test_monitoring: cascade fixed (tool events left set by a failing test).

## Known gaps / bugs
* Monitoring offsets are our instruction indices * 4; tests that compare CPython's exact
  offsets (BRANCH_OFFSET recorders) cannot match.
* f_lineno jumps: target = first instruction of the first line >= n with a compatible
  stack; no "can't jump from a 'yield'" check.
* SubinterpImportTests (sub-interpreters share all state), `_testinternalcapi` covers only
  what is listed above (no interpreters/crossinterp/opt helpers).
* Lazy imports: thread-safety of concurrent reification is best effort (no per-placeholder
  lock); repeated `lazy import a.b` / `lazy import a.c` on the same top name keep the first
  placeholder (second is tracked as a pending submodule).
* pyc magic has 10 values; a compiler change maps to a previous value with p=0.1.
