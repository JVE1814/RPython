# Workstream "containers" — notes and decisions

Owned files: `types/{list,list_methods,tuple,dict,dict_methods,dictview,set,slice,iterobject,
weakref,mappingproxy,range,enumerate,builtin_iters,property,frozendict,containers_reg}.rs`,
`modules/_weakref.rs`, `Lib/weakref.py`, `Lib/_weakrefset.py`.

## Registration
`types/containers_reg.rs` has the three hooks used by `types/mod.rs`
(`init_types`, `ready`, `builtin_types`). The temporary `bootstrap_builtins.rs` is gone.
`containers_reg::disallow_instantiation` gives non-instantiable static types a `tp_new`
raising "cannot create 'X' instances" (the core type_call does not look at
`Py_TPFLAGS_DISALLOW_INSTANTIATION`, and `tp_new` is inherited from object).
`__class_getitem__` uses lang's `genericalias::new_generic_alias`.

## Thread safety
`dict::ob_lock(o)` is a PyMutex-compatible spin/yield lock on `PyObject.ob_mutex`
(bit 0 = locked; never sets the "parked" bit). Used by dict, set and list mutations
and by dict/set reads. Rule: never hold it while Python code can run — key comparisons
with a Python `__eq__` drop the lock and re-validate with the table version; old
values are decref'd after unlocking. `list_items()` (used by interpreter fast paths) is
still an unlocked borrowed view; old item arrays are freed immediately (no deferred
reclamation yet — D6's safepoint-deferred free is future work).

## dict
* Table: `Vec<i32>` index array (power of two, perturbation probing like CPython) +
  insertion-ordered entry vector with holes; resize to `used*3` usable entries.
* `DictTable.version` is a globally unique stamp (`dict::next_version()`), renewed on
  *every* mutation incl. value replacement — the JIT's caches can key on `dict::version(d)`
  (the jit branch's `interp::cache::next_dict_version` can simply call `dict::next_version`).
* Iterators follow CPython: size check ("dictionary changed size during iteration"),
  remaining-count check ("dictionary keys changed during iteration"); popitem trims
  the entry vector, deletion leaves holes.
* Unhashable keys: TypeError is rewritten to "cannot use 'T' as a dict key (...)"
  (`dict::hash_key`/`hash_for`; also "set element", "frozendict key").
* `frozendict` (PEP 814): layout = PyDictObject + ma_hash, *not* a dict subclass,
  hash = hash(frozenset(items)), `frozendict(fd) is fd`, `|` returns frozendict.

## set
Real open-addressing table in the public PySetObject layout (9 linear probes then
perturbation, dummy entries, fill/used/mask, smalltable, finger for pop) plus a private
`version` after `weakreflist`. Iteration orders match CPython's algorithm.
frozenset hash = CPython's shuffle-bits algorithm.

## list
CPython growth pattern; `sort` is a timsort (runs, binary insertion, galloping merges,
powersort merge policy) with pre-scanned fast comparisons for all-str / all-int /
all-float keys; during sort the list is empty with `allocated == -1`
("list modified during sort").

## weakref
`PyWeakReference` layout from the header; per-object list head at `tp_weaklistoffset` or
the managed pre-header slot (`Py_TPFLAGS_MANAGED_WEAKREF`); `type` objects use
`tp_weaklist` (set in `weakref::ready`). Basic ref/proxy reuse, callbacks run after all
refs are cleared, errors go to the unraisable hook.
**For the GC workstream:** `weakref::clear_weakrefs_for_gc(&garbage)` clears weakrefs to
all garbage first and only runs callbacks of weakrefs that are not themselves garbage
(CPython semantics); `gc.rs` currently calls `clear_weakrefs` per object.

## Observed problems in other areas (not fixed here)
* `gc::track` increments `ALLOCS_SINCE` but deallocation never decrements it, so every
  2000 tuple allocations trigger a full collection: `for t in d.items(): pass` is ~50x
  slower than it should be (CPython counts allocations minus deallocations).
* Static types without `tp_new` inherit object's `tp_new` (CPython instead marks them
  DISALLOW_INSTANTIATION).
