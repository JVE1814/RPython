# perf3 workstream — library and runtime hot paths

Branch: the perf3 worktree (based on master 991be0c, after jit5). Everything written from
scratch. Goal: the library/runtime-heavy pyperformance benchmarks where jit5's wall-clock
table showed the largest losses against the best CPython build.

## Method
* `tools/bench/perloop.sh` (callgrind instructions per benchmark loop, steady state) for
  ours, py-default and py-ft; per-loop *function* profiles by diffing two callgrind runs
  with different loop counts (scratchpad `dp.sh`), plus micro scripts measuring one
  operation (`(I(N2)-I(N1))/(N2-N1)`).
* Caveat: several benchmarks (gc_traversal, gc_collect, pathlib) vary by up to ~30% between
  runs of the same binary under callgrind (automatic collections and JIT recompiles land in
  different windows); numbers below are single runs unless noted.

## Changes

### binascii / bytes (base64)
* Inputs are borrowed (buffer export or the ASCII str's own bytes) instead of copied into
  a `Vec`; outputs are written straight into a bytes object allocated at the final (or an
  upper-bound, then shrunk in place) size (`emit`).
* Fast loops: base64 encode 3->4 with a fixed table, decode whole quads of alphabet
  characters before falling back to the general state machine (identical behaviour in
  every mode while no padding/invalid char has been seen); base32 groups of 5/8; hex via a
  256-entry digit table; ascii85/base85 groups without per-group vectors.
* `alloc::alloc_object_payload_uninit` + `bytes::try_alloc_bytes_uninit`: large bytes
  payloads that are overwritten anyway skip calloc's clearing (memset was 7% of base64).
  `new_bytes`/`try_new_bytes` use it.
* `bytes.translate`: 256-byte local map, branch-free mapping pass + compare when there is no
  delete set.

### str building / json
* `StrBuilder` collects one byte per character while everything is below U+0100 and only
  widens to `u32` on the first wider character (was always `Vec<u32>` + a max scan).
  Used by repr of containers, json, printf-style formatting, the C API writer.
* json encoder (`_json.rs`): circular-reference check with a private identity stack (set
  above depth 32) when the `markers` dict is empty at call start (the dict stays empty, as
  after any successful encode); exact dicts iterate an item snapshot without building
  2-tuples and a list; exact ints are written digit by digit, floats via `float_repr`
  without a temporary float object; plain-ASCII/Latin-1 strings are copied in runs;
  positional-argument fast paths for `make_encoder(...)` and `encoder(obj, level)`.
* `json.dumps`/`json.loads` (Lib/json/__init__.py): calls with no options go straight to
  the shared encoder/decoder without building option dicts.
* json scanner: key memo is a per-call Rust table (no dict + clear per call), objects are
  filled directly into the dict when there is no `object_pairs_hook`, machine-word ints are
  accumulated without a temporary `String`, positional fast path for `scan_once`.

### sets, min/max
* Lock-free set algebra (`set.rs`): when a table cannot change underneath us (frozensets;
  any set while only one thread exists) `issubset`, `difference`, `intersection`,
  `difference_update`, `update`/merge, copy and iteration walk the tables directly. Keys
  are compared without Python code when they are identical or both exact str/int; the
  first pair that would need `__eq__` makes the operation fall back to the old locking
  snapshot implementation. Results under construction get keys with `insert_unique`
  (no comparisons); `copy_fast` duplicates a table without dummies position by position.
* `min()`/`max()` without key over all-compact-int or all-float items compare natively.
* `collect_iterable` of exact sets/frozensets copies the table directly.

### calls and instantiation
* `type_vectorcall` handles one-argument `str(x)`, `type(x)`, `tuple(x)`, `list(x)`
  (exact-size copies of lists/tuples) and `weakref.ref(x)` without an argument tuple; the
  four types now dispatch through it (`types/mod.rs`).
* Classes whose `__new__` is a Python function (staticmethod, plain `type` metaclass):
  `__new__` and a Python `__init__` are called with the vector arguments
  (`instantiate_with_new`), no args tuple / kwargs dict / `slot_tp_new` getattr.
* LOAD_ATTR/LOAD_METHOD caches on classes (`specialize.rs`, flagged): non-data attributes
  of `type` itself (`__new__`, `__init__`, `mro`, ...) no longer prevent caching a class's
  own attribute of that name (`object.__new__(cls)` missed the cache on every call).
* CALL_FUNCTION_EX (eval and the tier-1/2 helper, flagged small changes): callables without
  vectorcall get the argument tuple itself (`abstract_::call_ex_tuple`), others a stack
  vector; LIST_EXTEND goes through `list::extend`, which copies tuples/lists directly.
* `list::list_to_tuple` (INTRINSIC_LIST_TO_TUPLE, `tuple(list)`, `sequence_tuple`),
  `list.extend(tuple)` without temporary vectors.
* tuple rich comparison compares the item slices directly.
* lru_cache: the argument tuple itself is the key when there are no keywords (as CPython).

### dicts
* Dicts created empty share one never-written table (`empty_table`, marked by capacity 0);
  the first write (anything going through `dict::table`) installs a private table with a
  compare-and-swap (a lock-free reader may race the writer). `find` reads the shared table
  without swapping. An unused `**kwargs` or `{}` no longer allocates a table. Sharing the
  version stamps among empty dicts is sound because their contents are identical.

### GC
* Trial-deletion state lives in the object headers during a collection (`ob_tid` =
  `gc_refs << 3 | reachable << 2 | tag`, owners restored afterwards) instead of index
  arrays; exact lists and tuples are traversed with the visitor inlined.
* The collector's helper sets/maps (`countable`, legacy finalizers, finalization order,
  weakref detaching) use the address hasher instead of SipHash (30% of gc_collect).

### Later additions
* `abstract_::plain_eq`: equality of identical objects, exact str/int/float and tuples (also
  tuple subclasses that keep tuple equality, e.g. namedtuples) of such, decided without
  Python code; used by dict lookup (the lock stays held), tuple comparison, `rich_eq` and
  the set fast paths. Tuple hashing skips the recursion guard for leading str/int items.
* Class-attribute (TYPE_ATTR) caches also hold method and slot-wrapper descriptors
  (`dict.__repr__`, `list.append` read through the class are those objects).
* Builtin keyword-argument parsing (`args::parse`) without a vector or a `String` per
  keyword; base64 decoding with a prebuilt default table.
* ElementTree tree builder keeps its text buffer capacity; expat end-tag check without a
  `String`.
* `copy.deepcopy` of lists/tuples/dicts skips the recursive call for atomic items.

### Bug fixed on the way
* `lru_cache` (bounded): the Rust node list was mutated by concurrent callers without any
  lock — heap corruption ("double free or corruption", "unaligned tcache chunk") in
  test_functools `test_lru_cache_threaded2` (rare before, frequent once the dict changes
  made calls faster). List and matching dict updates now run under the wrapper's lock once
  a second thread exists (released around the user function call).

## Measurements
Callgrind instructions per loop (millions, `perloop.sh`, final binary b051c32 unless noted).
"before" = master 991be0c built in this worktree. Several benchmarks vary 10-30% between runs
under callgrind (automatic collections, JIT recompiles); ranges are given where repeated runs
differed. ratio = ours / CPython default (the better baseline in instructions).

| bench | before | after | CPython default | ratio after |
|---|---|---|---|---|
| base64 | 1284.3 | 463.6 | 570.9 | 0.81 |
| json_dumps | 171.5 | 65.7-94 | 102.1 | 0.64-0.92 |
| json_loads | 7.23 | 5.48-5.51 | 5.70 | 0.97 |
| gc_traversal | 70.8 | 22-33 | 54.0 (ft 13.4) | 0.4-0.6 |
| gc_collect | 3.8-4.3 (dp) | 2.8 (dp) | 9.2 | 0.31 |
| meteor_contest | 1527.9 | 994-1009 | 850.8 | 1.17-1.19 |
| mdp | 18206 | 14547 | 10964 | 1.33 |
| pprint | 34469 | 30507 | 24396 | 1.25 |
| xml_etree (all four) | 5442 | 5156 | 4076 | 1.27 |
| xml_etree parse only | 5375 | 5140 | 4069 | 1.26 |
| typing_runtime_protocols | (noisy) | 2.53-2.70 | 1.73 | 1.46-1.56 |
| pathlib | 115-201 (noisy) | 150-192 | 87.2 | 1.7-2.2 |
| deepcopy | 4.21 | 4.12-5.17 | 2.83 | 1.46-1.83 |
| logging | 1.76 | 1.69 | 1.05 | 1.61 |

Wall-clock (s/loop through `tools/bench/run_bench.sh`, ours / py-default / py-ft back to back
pinned to core 3, other agents running; jit5's ratio from its table):

| bench | ours | default | ft | ratio | jit5 ratio |
|---|---|---|---|---|---|
| base64 | 0.000289 | 0.000199 | 0.000220 | 1.45 | 2.98 |
| json_dumps | 0.00612-0.00670 | 0.00611-0.00632 | 0.00707 | 1.00-1.06 | 2.83 |
| json_loads | 1.74e-05 | 1.56e-05 | 2.12e-05 | 1.12-1.17 | 1.85 |
| gc_traversal | 0.00175 | 0.00267 | 0.00130 | 1.35 | 2.77 |
| gc_collect | 0.00105 | 0.00156 | 0.00129 | 0.81 | 1.49 |
| meteor_contest | 0.0986 | 0.1145 | 0.1128 | 0.87 | 1.83 |
| sqlite_synth | 2.02e-06 | 2.48e-06 | 2.52e-06 | 0.82 | 1.37 |
| typing_runtime_protocols | 0.000222 | 0.000119 | 0.000130 | 1.86 | 2.34 |
| pathlib | 0.0183 | 0.0093 | 0.0092 | 1.99 | 2.35 |
| deepcopy | 0.000282 | 0.000183 | 0.000204 | 1.54 | 1.86 |
| xml_etree | 0.200 | 0.130 | 0.152 | 1.53 | 1.71 |
| pprint | 0.971 | 0.697 | 0.703 | 1.39 | 1.66 |
| logging | 8.3e-06 | 4.3-6.1e-06 | 5.8-7.3e-06 | 1.38-1.93 | 1.39 |
| mdp | 2.06 | 1.25 | 1.72 | 1.65 | 2.05 |
| bpe_tokeniser | 6.45 | 3.62 | 4.35 | 1.78 | 1.65 |
| asyncio_tcp | 1.97 | 0.77 | 0.59 | 3.32 | 2.53 |
| python_startup | 0.0130 | 0.0090 | 0.0141 | 1.44 | 1.46 |

(bpe_tokeniser, asyncio_tcp, python_startup were not worked on; their wall numbers moved with
machine load.)

## What is left (measured causes)
* asyncio_tcp, bpe_tokeniser, python_startup: not investigated in this workstream.
* mdp / pprint: remaining gap is call overhead (h_call, run_call_frame, h_call_py,
  call_methoddef ~25% of pprint) and Fraction arithmetic in Python (mdp).
* xml_etree: the expat engine decodes into `Vec<char>` and allocates a `String` per name,
  attribute and text piece (and per newline), then a str per piece; malloc/free are ~18% of
  parse. A byte-slice based engine producing Python strs directly is the real fix.
* Python-level call overhead dominates pathlib, typing_runtime_protocols, deepcopy: a
  non-inlined Python call costs ~500 instructions here vs ~150-200 in CPython (h_call /
  call_function / bind_arguments / frame release / run_frame; jit5's open item "contiguous
  frame stack"). pathlib per path: 12.6K vs 5.7K instructions.
* Allocation: `alloc_object` ~100-110 instructions per object, `free_object`+`free_small`
  ~50; object-heavy benchmarks spend 10-15% there.
* gc_traversal vs py-ft: ft marks everything reachable from the roots (including thread
  stacks) in one pass; we still do two passes over all tracked objects.
