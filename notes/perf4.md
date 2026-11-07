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

### Second round
* Builtin calls from compiled code (`h_call` → `call_builtin_fast`): exact
  `builtin_function_or_method` objects and method descriptors found by LoadMethod with
  METH_O / METH_NOARGS / METH_FASTCALL(|KEYWORDS) and positional arguments call their C
  function directly (`methods::call_methoddef_fast`), skipping vectorcall_raw,
  cfunction_vectorcall, call_methoddef and the result check (locking methods and every
  other convention keep the general path, which also produces the errors).
* `list.copy()` / `list(lst)`: no intermediate vector (`list::list_copy`).
* Exceptions leaving a compiled frame that has no handler for the raising instruction are
  propagated without entering the interpreter (`eval::unwind_compiled_error`, used by
  `jit::finish_in_interp`): context chaining, traceback entry and propagation mark as in
  `eval_inner`'s unwinding.
* A frame object that owns its finished frame (tracebacks) gives the frame back to the
  frame pool when it dies (a traceback cost a fresh `Box<Frame>` + slot vector for a later
  call).
* `obj.__eq__(x)` / `object.__init__(self)`-style calls of slot wrappers (method-wrapper,
  wrapper_descriptor) go to the wrapper without the tp_call argument tuple
  (`descr::call_wrapper_fast` from `vectorcall_raw`).
* `alloc_object` fast path restricted to blocks up to 512 bytes (zeroing needs no library
  call) with the heap-type reference in a tail call: the fast path saves no registers.
* `__dict__` materialisation (shape.rs) moves the inline values into the new dict with one
  bulk fill under one lock (`dict::fill_fresh_steal`; the dict is published before the
  slots are cleared, so lock-free readers never see a hole).
* Bytes/bytearray copies: bytearray growth and front deletion copy into uninitialised
  storage (no calloc zeroing of the part that is copied over, no temporary vector);
  growth for slice assignment/extend does not zero the bytes it overwrites
  (`raw_resize_z`); `bytearray.extend(bytes)` without a temporary copy; `bytes[a:b]`,
  `bytes(buffer)`, bytes concatenation and contiguous memoryview copies copy once into
  uninitialised storage; `socket.recv(n)` (n > 2 KiB) receives straight into an
  uninitialised bytes object that is shrunk in place.
* Lib/asyncio: the socket transport keeps exact `bytes` passed to `write()` without a
  private copy (they cannot change); `StreamReader._consume` copies once
  (`bytes(memoryview(buf)[:n])`, or the whole buffer + clear).
* The executable exports only the C API (`crates/launcher/exports.map`, version script:
  `Py*`, `_Py*`, `PY_*`, `RPY_*`); `--export-dynamic` alone exported ~3700 internal Rust
  symbols (.dynstr 325 KB -> 36 KB). `RPY_EXPORT_ALL=1` at build time restores the old
  behaviour.

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
* Descriptors of static types are not GC-tracked (they refer only to their never-freed,
  untracked type and to strings, so they cannot be part of a cycle): every collection
  visited ~5,500 of them (start-up 57.7M -> 56.6M instructions, mostly the collections at
  shutdown).
* Public code objects keep no per-instruction `Location` table (16 bytes per internal
  instruction): frame lines come from `pub_lines`; `line_for_instr` and f_lineno jumps
  fall back to it (RssAnon at `-c` 4.10 -> 3.95 MB).

## Measurements
"base" = master 6431748 built in this worktree; "perf4" = the final branch head (installed
snapshots, same Lib except the files changed here). CPython per-loop numbers marked
"(jit3)" come from the jit3/perf3 tables (same harness), the others were measured here.

### Calls (instructions per call, `percall.sh`, includes the loop)
| kind | base | perf4 | CPython default |
|---|---|---|---|
| kw `f(i, b=2)` | 988 | 849 | 701 |
| defaults `f(i)` | 421 | 421 | 638 |
| star `f(*t)` | 2520 | 1542 | 856 |
| kwargs `f(**d)` | 4636 | 2775 | 1818 |
| init `C(i)` | 845 | 754 | 1253 |
| super `super().__init__(x)` | 3509 | 2183 | 1911 |
| prop `o.p` | 702 | 641 | 441 |
| bound `m(i)` | 814 | 723 | 502 |
| map `list(map(f, data))` per item | 1026 | 663 | 542 |
| sortkey per item | 550 | 516 | 502 |
| dunder (`__hash__`+`__eq__` dict probe) | 2448 | 1822 | 1452 |
| getattr_call `getattr(o, 'm')(i)` | 1675 | 1389 | 1094 |
| alloc `(i, i); [i]` | 1627 | 960 | 762 |
| builtin `isinstance; id; l.copy()` | 1966 | 1308 | 1374 |
| exc (raise + catch one level down) | 5422 | 4635 | 3686 |
| keyerror (`d[k]` miss + except) | 2649 | 2258 | 1841 |
| counter (`stats[pair] += 1` over zip) | 3507 | 2445 | 2360 |

### Steady state per benchmark loop (callgrind, millions, `perloop.sh`)
| bench (windows) | base | perf4 | CPython default | perf4/CPython |
|---|---|---|---|---|
| deepcopy (20/120) | 4.07 | 3.52 | 2.83 (jit3) | 1.24 |
| logging (50/250) | 1.71 | 1.64 | 1.05 (jit3) | 1.56 |
| typing_runtime_protocols (50/250) | 2.61 | 2.22 | 1.73 (jit3) | 1.28 |
| pathlib (2/6) | 177.7 | 148.4 | 87.2 (jit3) | 1.70 |
| json_dumps | 94.3 | 83.2 | 102.1 (jit3) | 0.81 |
| json_loads | 5.69 | 5.20 | 5.70 (jit3) | 0.91 |
| richards | 264.9 | 264.0 | 461.1 (jit3) | 0.57 |
| deltablue | 30.7 | 28.1 | 29.1 (jit3) | 0.97 |
| go | 1094 | 1039 | 1138 (jit3) | 0.91 |
| nqueens | 954 | 845 | 842 (jit3) | 1.00 |
| chaos | 402 | 354 | 611 (jit3) | 0.58 |
| generators | 279 | 272 | 337 (jit3) | 0.81 |
| comprehensions | 0.23 | 0.21 | 0.19 (jit3) | 1.11 |
| float | 669 | 575 | 778 (jit3) | 0.74 |
| hexiom | 53.8 | 49.8 | 58.1 (jit3) | 0.86 |
| raytrace | 2543 | 2348 | 2757 (jit3) | 0.85 |
| unpack_sequence | 0.20 | 0.20 | 0.25 (jit3) | 0.80 |
| coroutines | 283 | 274 | 294 (jit3) | 0.93 |
| base64 | 460 | 456 | 571 | 0.80 |
| async_generators (1/2) | - | 3839 | 3871 | 0.99 |

(logging/typing/deepcopy windows are short: the benchmark's own outer loop still runs in
the interpreter for its first ~10000 iterations.)

asyncio_tcp, a 1/10 variant under callgrind (10 x 10 MB): 1607M -> 746M instructions
(memset 328M -> 13M, memcpy 768M -> 224M); wall s/loop 1.38 -> 0.85 (CPython 0.57).

### Start-up (callgrind instructions, whole process)
| | base | perf4 | CPython default |
|---|---|---|---|
| `-c pass` | 63.9M | 56.6M | 40.5M (perf2) |
| `-S -c pass` | 34.3M | 29.2M | 27.7M (perf2) |

### Memory
| | base | perf4 | CPython default | CPython ft |
|---|---|---|---|---|
| startup_mem.sh peak_rss_kb | 41100 KB | 34928 KB | 32280 KB | 38640 KB |
| `-c` VmRSS (RssAnon / RssFile) | 15.1 MB (4.2 / 10.9) | 11.0 MB (3.9 / 7.1) | | |

### Wall clock
s/loop through `tools/bench/run_bench.sh`, the four binaries back to back per benchmark,
pinned to core 3, one run each, other agents running (load 3-6): indicative only (D22;
single runs vary by ~10%, e.g. generators). ratio = x / best CPython.

| bench | base | perf4 | CPython default | CPython ft | base ratio | perf4 ratio |
|---|---|---|---|---|---|---|
| python_startup | 0.01397 | 0.01241 | 0.009739 | 0.01181 | 1.43 | 1.27 |
| deepcopy | 0.000266 | 0.0002294 | 0.0001519 | 0.0001832 | 1.75 | 1.51 |
| logging | 7.208e-06 | 5.832e-06 | 4.313e-06 | 6.03e-06 | 1.67 | 1.35 |
| typing_runtime_protocols | 0.0002026 | 0.0001576 | 0.0001103 | 0.0001411 | 1.84 | 1.43 |
| pathlib | 0.02006 | 0.01672 | 0.009483 | 0.01052 | 2.12 | 1.76 |
| json_dumps | 0.006186 | 0.005385 | 0.006491 | 0.007753 | 0.95 | 0.83 |
| json_loads | 1.841e-05 | 1.595e-05 | 1.677e-05 | 1.862e-05 | 1.10 | 0.95 |
| go | 0.08568 | 0.08064 | 0.07896 | 0.09112 | 1.09 | 1.02 |
| raytrace | 0.1814 | 0.1696 | 0.1852 | 0.2142 | 0.98 | 0.92 |
| richards | 0.01942 | 0.01882 | 0.03121 | 0.03668 | 0.62 | 0.60 |
| deltablue | 0.001988 | 0.001784 | 0.002094 | 0.002552 | 0.95 | 0.85 |
| nqueens | 0.07027 | 0.06443 | 0.05591 | 0.06525 | 1.26 | 1.15 |
| chaos | 0.03065 | 0.02641 | 0.04039 | 0.04324 | 0.76 | 0.65 |
| float | 0.05163 | 0.04697 | 0.05041 | 0.06144 | 1.02 | 0.93 |
| generators | 0.02426 | 0.02584 | 0.02071 | 0.0235 | 1.17 | 1.25 |
| coroutines | 0.02519 | 0.01404 | 0.01458 | 0.01426 | 1.77 | 0.98 |
| comprehensions | 1.545e-05 | 1.395e-05 | 1.04e-05 | 1.15e-05 | 1.49 | 1.34 |
| async_generators | 0.3585 | 0.284 | 0.2375 | 0.3052 | 1.51 | 1.20 |
| unpack_sequence | 3.205e-08 | 2.697e-08 | 2.719e-08 | 3.178e-08 | 1.18 | 0.99 |
| hexiom | 0.00385 | 0.003597 | 0.004944 | 0.00427 | 0.90 | 0.84 |
| fannkuch | 0.2593 | 0.2198 | 0.2277 | 0.2928 | 1.14 | 0.97 |
| meteor_contest | 0.09414 | 0.09207 | 0.07864 | 0.09542 | 1.20 | 1.17 |
| gc_collect | 0.0007984 | 0.0006256 | 0.0009382 | 0.0009083 | 0.88 | 0.69 |
| gc_traversal | 0.001846 | 0.001634 | 0.002735 | 0.001123 | 1.64 | 1.46 |
| sqlite_synth | 1.697e-06 | 1.54e-06 | 1.865e-06 | 1.308e-06 | 1.30 | 1.18 |
| base64 | 0.0004198 | 0.0002527 | 0.0001994 | 0.0002104 | 2.11 | 1.27 |
| xml_etree | 0.1688 | 0.1488 | 0.1102 | 0.1026 | 1.65 | 1.45 |
| pprint | 0.8182 | 0.6204 | 0.5102 | 0.5576 | 1.60 | 1.22 |
| mdp | 1.564 | 1.429 | 0.9627 | 1.156 | 1.62 | 1.48 |
| bpe_tokeniser | 5.131 | 4.08 | 3.108 | 3.342 | 1.65 | 1.31 |
| asyncio_tcp | 1.555 | 0.917 | 0.5844 | 0.5547 | 2.80 | 1.65 |
| concurrent_imap | 0.03446 | 0.03583 | 0.02318 | 0.03119 | 1.49 | 1.55 |

## Correctness
tools/jit_tests/run.sh (with the new t_calls.py and t_exc_unwind.py) and tools/stress/run.sh
pass (shared_iterators.py aborts in ~1 of 8 runs on base and perf4 alike: pre-existing).
Spec modules run with tools/run_tests.sh on base and perf4 snapshots, identical results:
test_call test_extcall test_keywordonlyarg test_positional_only_arg test_super
test_linecache test_cmd_line test_gc test_dict test_descr test_class test_property
test_builtin test_itertools test_traceback test_exceptions test_list test_tuple test_set
test_funcattrs test_scope test_generators test_coroutines test_sys test_threading
test_weakref test_copy test_unpack_ex test_grammar test_opcache test_frame
test_sys_settrace test_inspect test_functools test_free_threading test_finalization
test_types test_dictviews test_ordered_dict test_bytes test_memoryview test_buffer
test_socket test_struct test_binascii test_base64 test_io test_zlib test_ctypes
test_sqlite3 test_ssl test_dbm test_bdb test_pdb test_monitoring test_lazy_import
test_dis test_code test_warnings test_pydoc and test_asyncio.{test_streams,
test_selector_events, test_sock_lowlevel, test_sendfile} (test_capi and the whole
test_asyncio package time out at 900 s under load for both).

## What is left (measured causes)
* Calls not inlined by tier 2 and not made by jit6's machine-code call routine still pay
  ~250-300 instructions in Rust (call_function -> call_fast/call_bound -> enter_bound ->
  run_call_frame -> trampoline -> recycle_call) against ~150 in CPython: kw 849 vs 701,
  bound 723 vs 502. Property getters (prop 641 vs 441) go h_load_attr -> prop_descr_get ->
  call_function: a tier-1 LoadAttr-of-property path through the callpy routine (like the
  Call sites) would remove most of it.
* `f(*t)`: our compiler emits BUILD_LIST/LIST_EXTEND/CALL_INTRINSIC_1(list_to_tuple)
  before CALL_FUNCTION_EX (a list + a tuple per call; star 1542 vs 856); `**kwargs`
  parameters need a fresh dict per call (kwargs 2775 vs 1818).
* Exceptions caught in compiled code: the activation is handed to the interpreter
  (eval_frame/eval_inner unwinding, then try_run re-enters at the handler), ~600
  instructions per caught exception (exc 4635 vs 3686, keyerror 2258 vs 1841); handler
  dispatch in tier-1 machine code would remove it. Hits typing_runtime_protocols
  (getattr_static raises AttributeError per protocol member miss) and deepcopy.
* logging: `record.__dict__` (`%`-style formatting) materialises the 22 inline attributes
  into a dict for every record (CPython keeps the values inline behind the dict).
* pathlib: ~18 Python calls per path (Path.__new__(*args, **kwargs), __init__ loop,
  with_segments via type(self)(*segments), DirEntryInfo) and ~20 objects per os.stat
  result; what remains is per-call and per-allocation overhead.
* Start-up 56.6M vs 40.5M instructions: public-to-internal bytecode translation (~10.5M for
  233 code objects, ~850 instructions per bytecode instruction, spread over the
  abstract-interpretation passes and allocations), class creation (fixup_slots for
  multiple-base classes ~100K each, readying 267 static types ~5M), three full
  collections at shutdown (~4.5M), unmarshalling (~8.7M). -S start-up is at parity in wall
  time; site's imports cost us ~2x CPython's instructions.
* Peak memory 34.9 vs 32.3 MB (default build; py-ft 38.6): the free-threaded object
  header costs ~2.4 MB on that workload's 100k ints and strs (48 vs 32 bytes per int,
  64 vs 56 per short str); start-up RSS 11.0 MB.
* Wall time above instruction ratio on several benchmarks (async_generators 0.99x
  instructions vs 1.2x wall, base64 0.80x vs 1.27x): not visible to valgrind. Measured
  candidates: every cross-CGU call is GOT-indirect (`-Z plt=yes -Z
  relax-elf-relocations=yes` makes them direct; go -4%, float -3% wall in a quick A/B -
  needs a `.cargo/config.toml` rustflags change, left to the coordinator); glibc's
  mmap/trim thresholds make large temporary buffers re-fault (base64: ~1500 minor faults
  per loop); raising them with GLIBC_TUNABLES removed the faults but raised the peak-RSS
  workload from 34.9 to 40.3 MB, so it was not done.
* xml_etree (1.45x): the expat engine still builds a Rust `String` per name/attribute/text
  piece (names are cached per parser in _elementtree, texts are not).
