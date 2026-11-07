# perf4 workstream — call path, allocation, start-up, memory

Branch: the perf4 worktree (based on master 6431748). Everything written from scratch.
Method as in perf3: callgrind instructions (primary; `tools/bench/perloop.sh` per benchmark
loop, `tools/bench/micro/percall.sh` per operation of a micro script), wall clock only
indicative (machine shared with 3 other agents, load 5-10).

New tooling:
* `tools/bench/micro/calls.py` + `tools/bench/micro/percall.sh`: instructions per call for
  one call shape (`kw`, `defaults`, `star`, `kwargs`, `init`, `super`, `prop`, `bound`,
  `map`, `sortkey`, `dunder`, `getattr_call`, `posmany`, `alloc`), (I(N2)-I(N1))/(N2-N1)
  after a warm-up run. Works for the baselines too (it is a benchmark script).
* `tools/jit_tests/t_calls.py`: binding shapes (keywords, defaults, *args/**kwargs, bound
  methods, every binding error) through the compiled call paths vs. the interpreter.

## Changes

### Allocation (alloc.rs, tlab.rs, object.rs)
* `alloc_object` fast path: ready type, no tracemalloc, block popped from the thread's
  free list. Kind / pre-header / initial GC bits come from two flag bits without
  branches; free-list slot from a precomputed `SLOT_OF[kind][size/8]` table (no rounding
  and no `kind * NCLASSES`, block size from `SLOT_SIZE[slot]`, no `% NCLASSES`); the
  header's flags/mutex/gc-bits/local-refcount words are one 8-byte store; everything else
  (`alloc_object_slow`) out of line.
* Block zeroing: LLVM had merged the per-size `write_bytes` arms back into one variable
  size `memset` call; now unaligned 32/16/8-byte stores in a short loop that the optimiser
  cannot turn into a call (inline asm for the vector stores).
* `free_object` fast path (tlab block, no tracemalloc): untrack = clear bits + counter,
  pre-header size from the managed bit; `free_small` fast path behind one per-thread flag
  (`fast_free` = registered, not exiting, no RPY_TLAB_CHECK); the rest out of line.
* `dealloc`: trashcan depth is a plain `#[thread_local]` (was `thread_local!` with
  `try_with` on every GC dealloc); deferral list touched only when something was deferred.
* `generic_free_and_type_decref` calls `free_object` directly when `tp_free` is the
  generic one (no indirect call).
* Size classes 512..2048 in 64-byte steps up to 1024, then 1280/1536/1792 (36 classes):
  e.g. the 800-byte item array of a 100-element list took a 896-byte block, 1100 bytes
  took 2048.
* BuildTuple/BuildList (interpreter and tier-1 helpers) move the items from the stack into
  the new object (`tuple::new_tuple_steal`, `list::new_list_steal`): no `Vec<PyRef>`
  (malloc + free per tuple/list display).
* `map`/`zip` `__next__` collect the items in a stack buffer (`next_items_buf`): no two
  vectors per item; zip refills its reused result tuple from the buffer.

### Calls (interp/mod.rs, interp/jit/mod.rs, abstract_.rs, helpers.rs, ...)
* `jit::call_bound` / `jit::run_bound`: a Python function with compiled code is entered
  directly for every argument shape (keywords, defaults, *args, **kwargs, keyword-only):
  bind into a pooled frame, then `run_call_frame` (cheap `recycle_call`). Before, only
  exact positional calls skipped `run_frame` + `eval_frame` + `jit::resume`.
* `bind_arguments`: no incref/decref of the code object; positional call that relies on
  trailing defaults has its own fast path.
* CALL_FUNCTION_EX (`call_ex_tuple`) for Python functions and bound methods of them:
  `interp::call_function_ex` binds straight from the argument tuple and the kwargs dict
  (`bind_kwdict`: no kwnames tuple, no argument vector, no items snapshot; `**kw`
  parameter filled with `insert_unique`); a function whose only parameter is `*args` gets
  the caller's tuple itself. Anything unusual (non-str keys, binding errors) goes through
  the general path, which produces the errors.
* LOAD_SUPER_ATTR (interpreter + tier-1 helper): `super().name` / `super(C, o).name`
  with the builtin super is looked up on the MRO after C without creating the super object
  (`super_::lookup_fast`); for the method form a plain function / method descriptor is
  pushed unbound with `o` as self (no bound method object).
* `h_call`: a bound method of a Python function is unpacked into the stack (function +
  self) and called through `call_function` (no `method_vectorcall`).
* `method_vectorcall` uses a stack buffer instead of a `Vec` when it cannot use the
  PY_VECTORCALL_ARGUMENTS_OFFSET slot.
* `vectorcall_raw` (every call from Rust/C code: map, sorted key=, slot dunders, ...)
  dispatches plain Python functions straight to `call_function`; property getters too.
* `enter_recursive_call`: two compares on the fast path, limits/headroom handling out of
  line.
* `slot_tp_richcompare` used `intern_str("__eq__")` (allocate + hash + table lookup) on
  every comparison: cached names. `intern_str` of an ASCII name looks the interned
  instance up without allocating (`intern_latin1`).
* Exceptions of types with BaseException's own `__new__`/`__init__` (KeyError,
  IndexError, ... and Python subclasses that do not override them) are built directly by
  `make_exception` (no type-call machinery, no argument vectors).
* `getattr(cls, name, default)` / `hasattr(cls, name)` on classes: no AttributeError is
  raised and cleared for a missing name (`type_getattr_impl` with suppression).

### Start-up and memory
* `python -c`: the command's source is no longer registered with linecache at start (that
  imported linecache, ~3.4M instructions, 1 ms): it is kept by the runtime and linecache
  registers it when it is first imported (`_imp._rpy_linecache_pending`, Lib/linecache.py).
* importlib bootstrap: the `.py` source is read only when the `rpython-boot` cache misses
  (it was read in full - 85 KB - on every start).
* One `pthread_getattr_np` per thread state (it reads /proc/self/maps for the main thread;
  was called twice).
* No automatic collections during start-up (`gc::startup_complete` at the end of
  `initialize`): start-up builds long-lived module state, and every collection walks the
  whole heap (2 collections, ~1.7M instructions).
* Dict growth like CPython: new index size >= 3 * used (it was 4.5 * used, so a dict of
  100k entries took 10.5 MB instead of 5.2 MB).
* Regenerated the start-up code order (`crates/launcher/startup.ld`, perf2's
  gen_order.sh): RssFile at `-c` 10.9 -> 6.8 MB.

## Measurements

### Calls (instructions per call, `percall.sh`, includes the loop)
| kind | base | perf4 | CPython default |
|---|---|---|---|
| kw `f(i, b=2)` | 988 | 822 | 701 |
| defaults `f(i)` | 421 | 426 | 638 |
| star `f(*t)` | 2520 | 1595 | 856 |
| kwargs `f(**d)` | 4636 | 2802 | 1818 |
| init `C(i)` | 845 | 770 | 1253 |
| super `super().__init__(x)` | 3509 | 2192 | 1911 |
| prop `o.p` | 702 | 624 | 441 |
| bound `m(i)` | 814 | 698 | 502 |
| map `list(map(f, data))` per item | 1026 | 647 | 542 |
| sortkey per item | 550 | 529 | 502 |
| dunder (`__hash__`+`__eq__` dict probe) | 2448 | 1791 | 1452 |
| getattr_call `getattr(o, 'm')(i)` | 1675 | 1522 | 1094 |
| alloc `(i, i); [i]` | 1627 | 1001 | 762 |

### Start-up (callgrind instructions, whole process)
| | base | perf4 | CPython default |
|---|---|---|---|
| `-c pass` | 63.9M | 58.4M | 40.5M (perf2) |
| `-S -c pass` | 34.3M | 29.5M | 27.7M (perf2) |

### Memory (tools/bench/startup_mem.sh peak_rss_kb workload)
| | base | perf4 | CPython default | CPython ft |
|---|---|---|---|---|
| peak RSS | 41100 KB | 34744 KB | 32280 KB | 38640 KB |
