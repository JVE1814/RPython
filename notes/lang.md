# Workstream "lang": advanced language features + typing (notes)

## Files owned
- `crates/runtime/src/types/exceptiongroup.rs` — BaseExceptionGroup/ExceptionGroup
  (constructor rules/messages, `ExceptionGroup(BaseExceptionGroup, Exception)` bases,
  derive/split/subgroup with metadata + `__notes__` copy, str/repr incl. the 3.14
  "repr of the original sequence" rule, `__class_getitem__`), `eg_match` (CHECK_EG_MATCH)
  and `prep_reraise_star` (re-raise projection by leaf identity; a single raised
  exception is not wrapped).
- `types/genericalias.rs` (new) — `types.GenericAlias`: repr, eq/hash, call (sets
  `__orig_class__`), attribute forwarding with CPython's exception list, `__dir__`,
  starred iterator (`*tuple[int]`), `__unpacked__`, `__typing_unpacked_tuple_args__`,
  pickling (`__reduce__`, iterator `__reduce__`), parameter collection/substitution
  (`make_parameters`, `subs_parameters`, shared with UnionType), `__class_getitem__`
  on tuple/list/dict/set/frozenset/slice/staticmethod/classmethod/generator/coroutine/
  async_generator + (late, by name) enumerate/memoryview/frozendict/mappingproxy.
  `type[int]` is special-cased in `abstract_::getitem` like CPython's PyObject_GetItem.
- `types/union.rs` — `types.UnionType` == `typing.Union` (tp_name `typing.Union`):
  `|` for types/aliases/unions, `Union[...]` (`__class_getitem__`) with
  `typing._type_check` for non-unionable members, flattening, hashable/unhashable
  de-duplication, eq/hash ("union contains N unhashable elements"), substitution,
  isinstance/issubclass via `__instancecheck__`/`__subclasscheck__`, pickling via
  `operator.getitem(Union, args)`.
- `types/typevar.rs` — the `_typing` module objects: TypeVar, ParamSpec (+args/kwargs),
  ParamSpecArgs/Kwargs, TypeVarTuple, TypeAliasType (qualname, lazy `__value__`,
  `evaluate_value`), Generic (class_getitem/init_subclass call into typing.py),
  NoDefault, `_typing._ConstEvaluator`; all interpreter intrinsics (`make_typevar` ...,
  `subscript_generic`, `make_typealias`, `set_default`). Lazily evaluated
  bound/constraints/default (PEP 749 `evaluate_*`). `_typing` builtin module registered.
- `types/asyncgen.rs` — async generator protocol: `__anext__`/asend/athrow/aclose
  awaitables with CPython's state machine and error messages, wrapped values
  (Intrinsic AsyncGenWrap), firstiter/finalizer hooks, unawaited-method RuntimeWarnings,
  `sys.get/set_asyncgen_hooks`, `sys.set/get_coroutine_origin_tracking_depth`, `cr_origin`
  (origin stored in the coroutine's otherwise unused `finalizer` field).
- `types/lang_util.rs` (new) — helpers for "header + pointer fields" types
  (generic dealloc/traverse/clear, `ready_type` sets DISALLOW_INSTANTIATION when no tp_new).
- `modules/annotations.rs` — `type.__annotations__/__annotate__` (reads `__annotate__`,
  then `__annotate_func__`; caches in `__annotations_cache__`; set/delete semantics),
  module `__annotations__/__annotate__` (not cached while `__spec__._initializing`).
- `modules/traceback_print.rs` — fallback printer now follows TracebackException.format:
  chains (cycle-safe), notes, exception groups with the `+-+---- n ----` frames,
  max width 15 / depth 10.
- `Lib/typing.py`, `Lib/annotationlib.py`, `Lib/__future__.py` — written from scratch.

## Small edits in shared files (minimal, needed for correctness)
- `interp/eval.rs`: bare `raise` keeps the traceback (no new entry, like CPython);
  RERAISE no longer overwrites `f.lasti` (the handler lookup must use the RERAISE
  position — previously any raise inside an except block looped forever);
  LOAD_GLOBAL on dict-subclass globals goes through `__getitem__` (`__missing__`),
  needed by annotationlib's fake globals.
- `types/genobject.rs`: `g`, `yieldfrom`, `gen_throw_impl`, `get_frame`, `get_suspended`
  made `pub`; async-gen finalizer called also for unstarted generators; `__await__`
  returning a plain generator is fine (only iterable coroutines are rejected; the old
  check leaked an exception); `cr_origin`; message "async generator already executing".
- `types/slots.rs::add_operators`: no slot wrappers for slots inherited by heap types
  (they caused infinite recursion: `class G(F)` with F having `__slots__` + `__init__`).
- `types/object_type.rs`: a `__doc__` entry in `__slots__` keeps its member descriptor;
  `__class__` assignment between types adding the same `__slots__` to the same base;
  immutable-type error message uses the full tp_name.
- `types/typeobject.rs::type_ready`: honours Py_TPFLAGS_DISALLOW_INSTANTIATION.
- `abstract_.rs` (`type[...]`), `builtins.rs` (one call), `modules/sys.rs` (one call),
  `types/mod.rs` (`lang_init`), `modules/mod.rs` (`_typing`).

## Decisions
- typing.py imports annotationlib lazily (`_lazy_annotationlib`), and creates
  ForwardRef/Pattern/Match/ContextManager/AsyncContextManager lazily in module
  `__getattr__` (as CPython 3.14), so `import typing` does not import re/contextlib.
- NamedTuple: the 3.15 signature `NamedTuple(typename, fields, /)` (keyword form removed).
- annotationlib STRING/FORWARDREF: own `_Stringifier` that renders source text directly
  with precedence-aware parenthesisation (no `ast` dependency). For class annotate
  functions the `__classdict__` free variable is replaced by the stringifier namespace.
  Real objects embedded in FORWARDREF expressions get `__annotationlib_name_N__` names
  (kept in `__extra_names__`), builtins are rendered by name.
- annotationlib is also listed under the harness workstream; this version is complete
  enough for typing; merge/coordinate if another one appears.

## Test status (run directly with local stub unittest/support, since the real
## unittest/collections/... were not available in this branch)
- test_except_star: 60/60 pass. test_exception_group: 50/54 (remaining: `_testcapi`,
  `__repr__` returning non-str message (abstract_.rs), stub artefacts).
- test_type_aliases: 32/34 (gc-dependent leak test; set ops).
- test_type_params: most failures are compiler scoping/mangling issues
  (class-scope `global`, `_Foo__T` mangling of type params, genexp in generic method),
  plus missing stdlib (inspect.signature, types.new_class/get_original_bases).
- test_asyncgen: all non-asyncio AsyncGenTest cases pass (asyncio missing).
- typing.py imports and works for the core (aliases, Generic, Protocol, runtime
  checks, NamedTuple, Literal, Annotated, substitution); `get_type_hints`/TypedDict
  blocked by missing `set`/`frozenset` operators in the runtime (`|`, `|=`, `<=`).

## Known gaps / blockers (other workstreams)
- set/frozenset operators (`|`, `&`, `-`, `<=`) missing — typing TypedDict,
  `_generic_init_subclass` subset checks, evaluate_forward_ref recursion guard.
- `re` (bytes iteration), `enum`, `collections`, `types`, `inspect`, `contextlib`,
  `pickle`, `unittest` not available yet in this branch's Lib.
- `ab::repr` lacks "X.__repr__() must return a str, not T" message.
- `type.__dict__` returns a dict instead of a mappingproxy.
- compiler: `inspect.signature(evaluator)` should show `(format=1, /)` (parameter name
  `format`, default 1) for compiler-generated annotate/evaluate functions.
