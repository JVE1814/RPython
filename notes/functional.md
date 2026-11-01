# Workstream "functional" — notes

## Rust modules (crates/runtime/src/modules/)
- `fncommon.rs` — shared helpers: `init_type`, `RObj<T>` (PyObject header + Rust state,
  generic dealloc/traverse via `Trav`), `VcArgs` (tp_new args -> FASTKW parsing),
  `reuse_tuple` (CPython-style result tuple reuse), `ReprGuard`, `class_getitem`.
  All our static types get `Py_TPFLAGS_IMMUTABLETYPE`.
- `functional.rs` — registration of all modules below (one line in modules::register_all).
- `_operator.rs` — all operator functions, attrgetter/itemgetter/methodcaller (vectorcall,
  `__reduce__`, `__text_signature__ = "(obj, /)"` on instances), `_compare_digest`,
  `length_hint`, `call`.
- `_functools.rs` — partial (Placeholder, flattening, vectorcall, descriptor binding,
  `__setstate__`/`__reduce__`, reentrancy-safe call/repr), Placeholder singleton, reduce,
  cmp_to_key (KeyWrapper), `_lru_cache_wrapper` (bounded LRU in a slab list, hash computed
  once and reused through known-hash dict APIs, reentrancy check).
- `_abc.rs` — `_abc_data`, `_abc_init/_abc_register/_abc_instancecheck/_abc_subclasscheck`,
  caches + negative cache + global invalidation counter, `__abc_tpflags__` handling,
  collection-flag propagation on register.  DEVIATION: registry/caches hold strong refs
  (no weakrefs in runtime yet) — switch to weakrefs when available.
- `itertools.rs` — every type incl. batched(strict=), pairwise (reentrancy semantics),
  tee/_tee/_tee_dataobject (iterative dealloc of long chains, "cannot re-enter"),
  groupby/_grouper.  No pickling (removed in 3.14), as test_itertools expects.
- `_collections.rs` — deque (VecDeque, maxlen, rotate, mutation detection, iterators
  `_deque_iterator`/`_deque_reverse_iterator` with `__reduce__`), defaultdict (C-level dict
  subclass), `_count_elements`, `_tuplegetter`.
- `_heapq.rs`, `_bisect.rs`, `atexit.rs` (run by sys_full::finalize via
  `atexit._run_exitfuncs`), `_contextvars.rs` (context in `ts.c.context`; mapping is a
  copy-on-write dict, not a HAMT), `_types.rs` (`_types` module: evaluates the same type
  expressions as types.py's fallback so identities match), `copyreg_support.rs`
  (object.__reduce_ex__ protocols 0-5, default __getstate__ incl. slots + "cannot pickle").
- `types/namespace.rs` — SimpleNamespace (`_PyNamespace_Type`, `new_namespace(kwds)`).

## Python modules (Lib/)
functools, operator, abc, _py_abc, collections (+abc), _collections_abc, keyword, reprlib,
types, copy, copyreg, heapq, bisect, contextlib, contextvars, enum, dataclasses.

## Decisions / caveats
- `_collections_abc` tolerates builtin iterator types the runtime cannot produce yet
  (helper `_iter_type`); harmless once the runtime is complete.
- OrderedDict: only the pure-Python implementation (no `_collections.OrderedDict` yet).
- enum.py and dataclasses.py follow CPython's documented internal contract closely because
  test_enum (`_test_simple_enum` compares class dicts) and test_dataclasses check internal
  names/reprs; comments were not reproduced.
- `types.SimpleNamespace` is `type(sys.implementation)` in the pure-Python fallback, so the
  sys owner should build sys.implementation with `types::namespace::new_namespace`.

## Status
Smoke-tested (own scripts): _operator, _functools, _abc/abc, itertools, _collections,
_contextvars, atexit, types, reprlib, keyword.  The spec test files could not be run yet:
runtime gaps outside this workstream (set operators, `open`, huge `range`, a
hash-seed-dependent hang in the import machinery) block unittest.
