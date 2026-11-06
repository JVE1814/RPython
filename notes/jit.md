# jit workstream — execution tiers, inline caches, allocator/GC/startup performance

Branch: `worktree-agent-adf8b04d272001116`. Everything here was designed and written
from scratch (no codegen crates, no reference to other implementations' source).

## Architecture (HotSpot-style tiers, DECISIONS D7)

| tier | where | what |
|---|---|---|
| 0 | `interp/eval.rs` (+ `interp/specialize.rs`, `interp/cache.rs`) | the interpreter; it is the reference semantics |
| 1 | `interp/jit/` | baseline "template" JIT for x86-64: every bytecode turns into a fixed machine-code sequence |
| 2 | `interp/jit/opt/` | optimizing JIT: typed IR from bytecode + profiles + inline caches, unboxing, guards with deopt, inlining (jit2 + jit3 sections below) |

### Tier 0 additions
* **Side tables per code object** (`cache.rs`, the HotSpot MethodData analogue). They
  are allocated when the code object first runs and published with release/acquire.
  Per instruction they hold an inline-cache entry and one byte of type profile. The
  bytecode itself stays immutable and shared between threads.
* **Cache entries** are small seqlocks (seq / kind / a / b / c):
  * readers check the sequence number around relaxed loads, so a torn entry is never
    used;
  * writers take the odd sequence number with a CAS;
  * there is no GIL.
* **Guards:**
  * *dict version stamps* (`next_dict_version`): globally unique, bumped on every
    mutation of a dict, handed out to threads in blocks;
  * *type version tags*: assigned lazily, reset by `slots::type_modified` on the type
    and all its subclasses.
* **Type dicts are watched.** The owning type is stored in the dict's `_ma_watcher_tag`,
  so even a direct `tp_dict` write (C code, Rust internals) calls `type_modified`.
* **Global method cache** (`cache::mc_lookup/mc_store`, used by `typeobject::lookup`).
  Key: (type tag, interned name) → result of the MRO lookup.
* **Cached opcodes:**
  * LoadGlobal: (globals version, builtins version) → value;
  * LoadAttr / LoadMethod / StoreAttr: polymorphic, 4 entries per site. Entry kinds:
    INSTANCE_DICT (with entry-index hint), SLOT (member offset), DATA_DESCR,
    CLASS_ATTR(_SHADOWABLE), METHOD(_SHADOWABLE), MODULE (module dict version),
    TYPE_ATTR. A per-site miss budget stops refilling megamorphic sites.
* **Fast paths:**
  * int arithmetic and comparisons on ≤2-digit ints, done in i64/i128;
  * float, mixed int/float, and str concatenation / equality.

### Tier 1 (baseline JIT)
* `x64.rs`: own assembler (REX/ModRM/SIB, SSE2, labels, fixups).
* `mem.rs`: W^X code memory. A memfd is mapped twice, once RW to write and once RX to
  execute, so code can be packed next to running code. Fallback: mprotect RW→RX.
* `analysis.rs`: decodes the bytecode and computes the static stack depth at every
  instruction.
* `tier1.rs`, the code generator. It uses the interpreter's frame layout (values live
  in `Frame::slots` at fixed depths). Registers: rbx = frame, r12 = slots, r13 = thread
  state, r15 = thread id, r14 = scratch.
  * Inline in the generated code:
    * stack/local traffic and biased reference counting (owner-thread fast path,
      atomic shared path);
    * eval-breaker polls at backedges and Resume (safepoints, GC, signals);
    * branches on bools;
    * compact-int and float arithmetic, plus fused compare-and-branch;
    * list/tuple subscripts;
    * ForIter over range/list/tuple. Those iterator layouts are private to their
      modules, so they are checked once at run time and the helper is used otherwise;
    * Yield, IsOp, ToBool/UnaryNot on bools.
  * Inline caches use the cache state observed at compile time:
    * polymorphic type-tag guards for instance-dict hint loads, slots, class/type
      attributes, methods (with a dict-shadow check) and slot stores;
    * module attributes and LoadGlobal read their guards from the cache entry at run
      time.
  * Per-function routines:
    * `box_int` / `box_float` allocate from the thread's free lists directly in
      machine code (small-int table for -5..1025);
    * `free_num` frees ints/floats whose refcount reached 0 straight back to the free
      lists.
  * Everything else calls helpers in `helpers.rs`. Their contract: inputs are consumed,
    outputs pushed, status returned. The semantics are tier 0's: eval.rs functions are
    shared, not duplicated.
* **Going back to tier 0 ("deopt")** is possible at any instruction boundary:
  * status 2: the interpreter continues at `f.next_instr` with `f.stack_top`;
  * status 1: an exception was raised and the interpreter unwinds; handlers are entry
    points again;
  * status 3: yield.
  Opcodes the JIT does not handle (with, match, import, raise/except bodies, async)
  simply deoptimise.
* **Entering tier 1:**
  * at function entry;
  * at loop headers (OSR from the interpreter's backedge);
  * at exception handlers;
  * after a yield (generator resumption).
  `eval_frame` enters compiled code directly (`jit::resume`), and
  `interp::call_function` does too for exact-positional calls (`jit::call_fast`), so
  neither path pays for the interpreter prelude.
* **Tiering policy:**
  * a code object is compiled after `RPYTHON_JIT_THRESHOLD` (default 100) calls, or 10×
    that many backedges;
  * compilation is synchronous, under a per-code lock (CAS on `tier_state`);
  * the code pointer is published with release, so other threads keep interpreting
    until it is visible;
  * only optimized function code is compiled; module and class bodies stay in tier 0.
* **Environment variables:**
  * `RPYTHON_JIT=0`: tier 0 only;
  * `RPYTHON_JIT_THRESHOLD=n`;
  * `RPYTHON_JIT_DEBUG=1`: log compilations;
  * `RPYTHON_JIT_DUMP=dir`: write raw machine code (inspect with
    `objdump -D -b binary -mi386:x86-64`).

### Runtime changes made for performance (outside interp/)
* `tlab.rs`:
  * per-thread intrusive free lists in a `#[thread_local]` (no RefCell, no Vec);
  * constant-size zeroing;
  * the layout is exported to the JIT (`jit_layout`).
* `gc.rs`:
  * generational: young list, survivors promoted, a full collection after 10 young
    ones once the heap has grown by half;
  * the graph index is stored in the GC pre-header during a collection (no hash map);
  * pointer hasher;
  * the allocation counter is decremented on untrack;
  * spin lock for the list.
* `types/dict.rs`:
  * version stamps and the hook for type-dict writes;
  * storage layout (assigned to this workstream): the `DictTable` header is a TLAB
    block, and indices plus entries share one data block (TLAB when ≤2 KiB, else
    malloc). A new dict is 2 small-object allocations instead of a Box and two Vecs
    (3 mallocs).
  * `entries_p`, `nentries` and `version` sit at fixed offsets for JIT code;
    `entries()` / `entries_mut()` return slices.
  * Exception/cycle loop (100k iterations): 1.28G → 1.10G instructions.
  * Spec tests unchanged against master: test_dict 7 failures, test_ordered_dict 16,
    test_weakref 4, test_gc 15 (identical failure lists); test_dictviews and test_set
    pass.
* `types/str.rs`: interning keyed on the cached hash (no key copies).
* `types/code.rs`: `new_code_from_parts` builds code objects directly from objects (used
  by marshal).
* `modules/marshal.rs`: `no_debug_ranges` is looked up once; no string round trips.
* `import.rs`: the compiled importlib bootstrap is cached in
  `Lib/importlib/__pycache__/*.rpython-boot`, keyed on source and executable stamps.
* `types/slots.rs`: per-slot name tables built once.
* `interp/frame.rs`: frames borrow the function's globals/builtins.
* Bug fixes found on the way: inherited slot wrappers on heap subclasses (since
  superseded by master's fix); a list `__getitem__` recursion (reported; fixed in master).

## Testing
* `tools/jit_tests/run.sh PY`: differential tests. Every `t_*.py` must print identical
  output with `RPYTHON_JIT=0` and with the JIT compiling at thresholds 0, 2 and 50.
  Coverage:
  * arithmetic, including bigints and mixed types;
  * calls, kwargs, closures, super;
  * exceptions in loops and tracebacks;
  * generators: send/throw/close and yield-from chains;
  * inline-cache invalidation after warm-up (class patching, shadowing, `__slots__`,
    module and global rebinding, builtins patching);
  * iteration with mutation, subscripts, float NaN comparisons;
  * GC cycles with finalizers across generations.
  All pass.
* Spec tests via `python -m test` that run on the current stdlib: test_bool, test_set,
  test_tuple and test_enumerate pass. Most others cannot be imported yet because of
  missing stdlib modules (doctest, py_compile, ...).

## Measurements
Tools:
* `tools/bench/micro/run.sh [-c] PY`: micro benchmarks; `-c` gives callgrind
  instruction counts.
* `tools/bench/jitbench.sh [-c] PY OUT`: pyperformance through `tools/bench/jitshim`, a
  pyperf stand-in that needs only sys/time.
* `tools/bench/fixedwork.sh`.
The machine is shared and heavily loaded (load average 20–30 on 4 cores), so
instruction counts are the primary metric.

Startup (`-c pass`, instructions): 135M at the master merge → **45M** now
(CPython default 40.5M). With `-S`: **21M** (CPython 27.7M).

Micro benchmarks (callgrind instructions, whole process; subtract startup: ours ~49M,
CPython ~40M):

| bench | CPython default | ours (start) | ours (now) |
|---|---|---|---|
| attr (100k) | 112.6M | 198M | 99.4M |
| fib(20) | 51.3M | 41M | 65.6M |
| floatops (100k) | 143.5M | 309M | 131.5M |
| forrange (300k) | 190.8M | 300M | 162.6M |
| gen (100k) | 102.0M | 163M | 112.1M |
| globals_ (100k) | 155.7M | 348M | 201.8M |
| listdict (100k) | 155.7M | 247M | 166.4M |
| loop (300k) | 225.4M | 461M | 157.3M |
| method (100k) | 120.0M | 296M | 168.9M |
| unpack (100k) | 132.3M | 384M | 153.9M |

The "start" column predates the stdlib-heavy startup (6.6M then). Net of startup, attr,
floatops, forrange, loop, gen and unpack now use fewer instructions than CPython;
fib/method/globals_ (call-bound) are about 1.3–1.5× CPython.

pyperformance (`jitbench.sh -c`, one loop, whole-process instructions in millions;
ratio = ours / CPython default):

| bench | CPython default | CPython ft | ours | ratio |
|---|---|---|---|---|
| chaos | 835 | 919 | 1332 | 1.60 |
| coroutines | 337 | 353 | 541 | 1.61 |
| deltablue | 102 | 120 | 153 | 1.50 |
| fannkuch | 4446 | 5020 | 6521 | 1.47 |
| float | 819 | 953 | 1108* | 1.35 |
| generators | 1029 | 1146 | 2174* | 2.11 |
| go | 1201 | 1339 | 2204 | 1.84 |
| hexiom | 133 | 155 | 194 | 1.45 |
| nbody | 1170 | 1452 | 1410 | 1.21 |
| nqueens | 886 | 991 | 1588 | 1.79 |
| richards | 527 | 654 | 627 | 1.19 |
| spectral_norm | 1251 | 1467 | 1831 | 1.46 |
| unpack_sequence | 69 | 83 | 81 | 1.17 |

(*) measured after the GC trigger change; the other rows predate the last few JIT
improvements and the generational GC.

Wall-clock medians from the shim (seconds per loop) are much noisier because of the
load; same order: ours / default / ft.

| bench | ours | CPython default | CPython ft |
|---|---|---|---|
| nbody | 0.350 | 0.311 | 0.339 |
| richards | 0.235 | 0.154 | 0.154 |
| float | 0.507 | 0.219 | 0.255 |
| spectral_norm | 0.590 | 0.244 | 0.296 |
| unpack_sequence | 1.67e-7 | 2.02e-7 | 1.36e-7 |

Wall time is worse than instruction counts suggest. Our IPC is about half of CPython's:
cachegrind on generators shows 5× the D1 misses and 2× the branch mispredictions. The
causes are full-heap GC traversal (since made generational) and malloc-heavy dict
storage.

## jit2 workstream (tier 2 + call path), status at pause

Branch `worktree-agent-ae352a18b358600a1`.

### Done
* **Fast Python->Python calls** (tier 1): `Call` checks inline for a function whose
  code has a published fast entry (`CodeCaches::fast_entry/fast_argc/fast_nlp/
  fast_total`, set when tier 1 compiles a plain positional function) and calls
  `jit::h_call_py`: arguments and the callable reference are *moved* into the callee
  frame (no incref/decref pairs), the frame comes from a `#[thread_local]` pool
  (`frame::alloc_call` / `recycle_call`; no RefCell, no memset, no Vec resize), the
  recursion check is inline. Call micro (`f(i, 2)` in a loop, 300k): 268M -> 196M
  instructions net of startup (CPython 194M).
* **Type profiles** recorded by tier 1 (`Tables::profile` is now u16 per pc: operand
  A bits low byte, B high byte; `cache::prof`, plus `IT_*` iterator kinds for ForIter).
  Fast paths `or` the bits inline, slow paths call `h_prof` / `h_prof_iter`.
* **Tier 2** (`interp/jit/opt/`, ~4k lines, all ours):
  * `build.rs`: abstract interpretation of the bytecode (CFG, RPO, loop headers with
    assumed entry states iterated to a fixed point) into a typed low-level IR
    (`ir.rs`). Locals get a per-unit representation: boxed in the frame (always
    current) or unboxed i64/f64 in a home vreg (chosen from the types stored to them;
    frame slot synced lazily; definedness tracked statically, "maybe" via a None
    placeholder in the slot). Stack entries are virtual (borrowed local refs with no
    refcounting, owned temporaries, unboxed values, lazy conditions, constants) and
    are only materialised at joins/templates/deopt. Native: int/float arithmetic with
    overflow/zero guards (unboxed), int/float compares fused into branches, `is`,
    ForIter over range/list/tuple (range counter unboxed), list/tuple subscript, list
    store (per-object mutex), UnpackSequence of tuple/list, LoadGlobal folded to a
    constant, float `**` via libm. Everything else is emitted by the **tier-1 template
    generator embedded in tier 2** (`tier1::Gen::new_embedded`), so every opcode tier 1
    handles works in tier 2 from day one.
  * Deoptimisation: every guard carries a snapshot (stack + dirty unboxed locals);
    `codegen.rs` emits a stub per snapshot that boxes/increfs values back into the
    interpreter frame and resumes in tier 0 at the instruction start. Error / poll /
    template exits sync dirty locals first.
  * `regalloc.rs`: liveness + linear scan (rcx,rdx,rsi,rdi,r8,r9 caller-saved; r14,rbp
    across calls; xmm2..14), spill slots in a native frame. Boxing/decref slow paths
    are register-preserving routines, so only calls/templates clobber.
  * Units: standard (pc 0, after `RPYTHON_JIT2_THRESHOLD` calls, default 1000) and OSR
    (loop header; tier-1 backedge countdown -> `opt::h_backedge` compiles and jumps
    straight into the tier-2 entry).
  * Invalidation: folded globals watch the globals/builtins dicts (`_ma_watcher_tag ==
    1`); a write invalidates dependents (flag checked at entry, after templates, at
    backedges), unpublishes the code and republishes tier 1; invalidated code is
    freed with the code object. After 3 invalidations globals are no longer folded;
    after 200 deopts code is discarded; at most 4 compilations per code object.
  * Debug: `RPYTHON_JIT_DEBUG=1`, `RPYTHON_JIT2_IR=<qualname>|*` dumps IR + allocation.
* `tools/jit_tests/run.sh` now also runs tier 2 (thresholds 1/5/50) — all green.
  `tools/bench/perloop.sh`: steady-state instructions per benchmark loop
  ((I(L2)-I(L1))/(L2-L1)), removes startup and warm-up from comparisons.

### Status
The problems listed at the pause (crash, invalidation storms, stale locals, tier 2
opt-in) were fixed by jit3; see the next section.

### Steady-state per-loop instructions (millions, perloop.sh; before tier 2 / call path)
| bench | CPython default | ours (start of jit2) |
|---|---|---|
| richards | 462.4 | 545.6 |
| nbody | 1107.6 | 1287.3 |
| float | 785.7 | 964.8 |
| spectral_norm | 1200.2 | 1674.7 |
| fannkuch | 4407.4 | 6596.0 |
| hexiom | 58.0 | 67.8 |
| deltablue | 36.8 | 69.8 |
| chaos | 610.7 | 931.8 |
| coroutines | 302.0 | 487.0 |
| nqueens | 841.8 | 1543.4 |
| go | 1131.6 | 2070.9 |
| generators | 328.5 | 578.3 |
| raytrace | 2757.1 | 5706.6 |
| richards_super | 531.0 | 664.3 |
(unpack_sequence/comprehensions need larger loop counts: -a 100 -b 300.)

## jit3 workstream (tier 2 merged, on by default; attributes, inlining, call path)

Branch `worktree-agent-a03d5179d8d7c7ce8`.

### Merge of jit2 onto master's frame model
* One frame pool (master's `#[thread_local]` `POOL`), one ownership rule: call frames
  borrow `code`/`func` (`owns_fc = false`, `make_owned()` when a frame outlives the
  call). jit2's `alloc_call`/`recycle_call`/`FPOOL` are gone; `Frame::new_call_raw`
  (geometry cached per code object, only locals cleared) serves both the interpreter
  call path and the JIT; `h_call_py` leaves the callable in the caller's stack slot
  for the duration of the call (that reference keeps the borrowed frame valid).
* `frame::recycle_call`: lean release of frames that returned normally in compiled
  code (no frame object / trace function / locals mapping), falls back to
  `frame_finished` otherwise.

### Tier 2 fixes
* (dict, key) dependencies for folded globals: `cache::on_dict_write(d, key)` (key
  NULL = any key: clear/merge); `opt::DEPS` maps dict -> [(key, code)]; writes of other
  keys do not invalidate (no storms from module-level loops). Folding looks the name
  up directly (globals, then builtins: dependencies on both) and re-validates after
  registering; names the code itself rebinds (StoreGlobal) are not folded. Module
  attributes of folded module objects (`math.sqrt`) are folded the same way.
* Frame introspection: a template with stale unboxed locals publishes a site
  descriptor (`Frame::t2_site`, values in spill slots at `Frame::t2_sp`);
  `f_locals`/`locals()` box them into the slots (`opt::sync_frame`) and the code
  deoptimises when the template returns (writes through `f_locals` are honoured);
  returns sync unboxed locals when a frame object outlives the activation.
* Bugs fixed: spill slots read after pushes (sp_bias), snapshot values of calls /
  templates were not kept live across the clobber, template jump edges missing from
  the IR CFG (liveness; crash in bm_go), template-exit values not addressable
  (bm_spectral_norm), values word read on objects without a managed dict.
* The test_generators segfault noted at the pause no longer reproduces (it was not
  a JIT bug: master's stability fixes); tier 2 is now on by default
  (`RPYTHON_JIT2=0` disables it).
* Tiering: the first OSR also compiles the standard unit (a function with a hot loop
  called a few times then runs tier 2 on every call — nbody 1888M -> 367M per loop);
  backedge counters are weighted by loop length (`1 + len/32`), so huge loop bodies
  compile after a few iterations; tier-1 slow-path profiling is switched off while
  tier-2 code is installed (`CodeCaches::t2_prof`); deopt limit 64.
* `RPYTHON_JIT_DEBUG=2` logs every tier-2 deopt (`code -> pc`).

### New tier-2 compilation
* LoadAttr / StoreAttr / LoadMethod from the inline caches: monomorphic sites guard
  the type version tag once (`Ty::Tag`, forgotten at templates) and deoptimise;
  polymorphic sites switch on the tag (up to 4 cases). Inline values: values word
  + shape key check, direct loads/stores (single-threaded; the locked runtime path
  when `MULTI_THREADED`); attribute-adding stores (e.g. in `__init__`) compile to
  "shape == parent and room left -> store, shape = child" (`shape::
  transitions_adding`; StoreAttr cache entries now record the type in `c`); shape
  misses call the generic cached path (`h2_load_attr` / `h2_store_attr`) instead of
  deoptimising. Slots, class attributes (TYPE_ATTR / CLASS_ATTR) as constants.
  METHOD_SHADOWABLE: per-site memo of a shape known to lack the name.
* Inlining of small straight-line callees (constant functions, monomorphic methods,
  <= 48 instructions, no jumps/handlers/cells): callee locals are abstract values
  (no frame); a guard failing inside the callee rebuilds its frame and finishes it in
  the interpreter (`InlineDeopt`/`InlineError` snapshots -> `h2_run_inlined`), then
  compiled code continues after the call with the result, so exceptions and
  tracebacks are unchanged. A callee that would need a template is not inlined (the
  attempt is rolled back). `RPYTHON_JIT2_INLINE=0` disables it.
* `len()` of list/tuple/dict/str inline (unboxed result).
* setcc for integer SetCond; branches use the compare directly when possible.
* Register allocation: bitset def sets, binary search for clobber crossings
  (compile time of big units).

### Tier 1
* LoadMethod on inline-values objects no longer calls the helper: the cache entry
  memoises the id of a shape known to lack the name (`Entry::memo`; shape ids are
  unique and shapes immutable). StoreAttr of an existing inline attribute is done in
  place (single-threaded).

### Testing
* `tools/jit_tests/run.sh PY`: 15 differential scripts (new: t_globals, t_introspect,
  t_attrs, t_tjump, t_inline, t_builtins, t_floatloops), 6 configurations (tier 1 at
  thresholds 0/2/50, tier 2 at 1/5/50): all pass. tools/bench/perf/t_shapes.py and
  t_threads.py pass under the same configurations.
* `tools/jit_tests/smoke.sh PY OUT`: spec smoke with JIT off / default / eager tier 2;
  final run (14 modules: test_bool test_set test_list test_dict test_generators
  test_coroutines test_descr test_class test_exceptions test_scope test_math
  test_float test_int test_threading): identical results in all three
  configurations. Pre-existing, not JIT-related (same with RPYTHON_JIT=0): test_descr
  crashes (refcount underflow in type_dealloc -> tuple_dealloc), test_class crashes
  (modules/testinternalcapi.rs has_inline_values dereferences the tagged values word
  as a dict), test_threading times out; failures in test_list 1, test_dict 7,
  test_generators 5, test_coroutines 6, test_exceptions 14, test_math 2.

### Steady-state instructions per loop (millions, `tools/bench/perloop.sh`)
Callgrind instructions per benchmark loop, steady state ((I(L2)-I(L1))/(L2-L1)), via
tools/bench/jitshim. Ours: -a 2 -b 5 (deltablue/hexiom -a 5 -b 15, unpack_sequence
-a 2000 -b 6000, comprehensions -a 1000 -b 3000, fannkuch 1/3); CPython 1/3
(unpack_sequence/comprehensions 100/300). "start" = merged jit2 + master with tier 2
on, before the jit3 work (tier 1 alone at that point: richards 594, nbody 1888,
fannkuch 9209, hexiom 122). ratio = ours / best CPython build.

| bench | CPython default | CPython ft | ours start | ours now | ratio |
|---|---|---|---|---|---|
| richards | 461.1 | 565.8 | 573.0 | **353.2** | 0.77 |
| nbody | 1107.5 | 1401.5 | 1888.7 | **393.7** | 0.36 |
| spectral_norm | 1207.8 | 1403.9 | 1130.5 | **818.4** | 0.68 |
| float | 778.4 | 870.1 | 929.2 | 865.6 | 1.11 |
| richards_super | 530.4 | 639.0 | 728.4 | 559.4 | 1.05 |
| chaos | 610.7 | 682.6 | 878.4 | 722.6 | 1.18 |
| raytrace | 2757.1 | 3200.8 | 5099.7 | 3640.1 | 1.32 |
| hexiom | 58.1 | 65.7 | 126.0 | 78.2 | 1.35 |
| deltablue | 29.1 | 42.4 | 72.4 | 49.3 | 1.70 |
| go | 1138.4 | 1259.6 | 2301.6 | 2110.8 | 1.85 |
| fannkuch | 4392.7 | 4962.9 | 8652.5 | 8646.7 | 1.97 |
| coroutines | 294.4 | 299.9 | 605.2 | 605.2 | 2.06 |
| generators | 337.1 | 372.4 | 768.6 | 810.3 | 2.40 |
| nqueens | 841.8 | 942.5 | 2080.1 | 2085.9 | 2.48 |
| comprehensions | 0.19 | 0.24 | 0.43 | 0.42 | 2.2 |
| unpack_sequence | 0.25 | 0.15 | 1.36 | 0.37 | 2.5 |

Small benchmarks (deltablue, hexiom) still include some compilation in the measured
loops (tier 2 compiles after 1000 calls); their numbers vary by 10-20% between runs.
The remaining gaps are generators/coroutines (resume path), runtime allocation/GC
costs (fannkuch, nqueens) and call-heavy OO code with branchy methods (go, raytrace,
deltablue: callees with branches are not inlined yet).

### Known gaps / next steps
* Generators and coroutines are not compiled by tier 2 and each resumption goes
  through gen_send_ex -> eval_frame -> jit::resume -> run -> trampoline (~150
  instructions per level of a `yield from`/`await` chain vs ~40 in CPython):
  generators, coroutines, nqueens, comprehensions are ~2x CPython. Needs a leaner
  resume path (direct entry from Send/ForIter into the generator's compiled code)
  and generator frames in tier 2.
* Runtime costs outside the JIT dominate fannkuch / nqueens: list item arrays and
  frames' slot Vecs via malloc, GC track/untrack under a CAS spin lock
  (`gc::with_list`; could be lock-free while single-threaded if the parent thread
  set MULTI_THREADED before spawning), slice objects GC-tracked.
* Inlining: callees with branches / calls (nested inlining needs multi-frame deopt),
  virtual tuples (BuildTuple + UnpackSequence), more builtins (isinstance, min/max).
* Tier-1 opcodes that still deoptimise to the interpreter (with, match, import,
  raise/except bodies, async) — unchanged.
* Each tier-2 unit carries its own copies of the boxing/decref routines (~2.4 KB per
  unit): share them per process.

## jit4 workstream (generators, deeper inlining, tier-1 coverage)

Branch `worktree-agent-a478005f2fd2e5fa4`. Note: jit3's work was on branch `core`, not
yet in master when jit4 started; it was merged from there (master has it since).

### Tier 1
* Instructions that used to deoptimise (exception handlers: PushExcInfo/PopExcept/
  CheckExcMatch/CheckEgMatch; with/async with; GetAwaitable/GetAiter/GetAnext/
  EndAsyncFor/CleanupThrow; ImportName/ImportFrom; Match*; LoadFromDictOrDeref,
  DeleteDeref, DeleteGlobal, LoadBuildClass, BuildTemplate/BuildInterpolation, GetLen,
  LoadCommonConstant) now run compiled. Their bodies moved out of `eval_inner` into
  `eval::exec_misc` (`is_misc`), which the interpreter dispatches to and tier-1 code
  calls through `helpers::h_misc` (stack_top set to the static depth while it runs and
  restored afterwards; on error the stack is cut to the depth the unwinder expects).
  RaiseVarargs/Reraise still deoptimise (the interpreter unwinds; handlers are entry
  points, so the handler body runs compiled again). `RPYTHON_JIT1_MISC=0` turns the
  shared path off. GetAwaitable has its own helper (a coroutine not suspended in an
  await is its own awaitable).
* Generator resumption without the interpreter prelude: compiled `YieldValue` stores
  the machine address of its continuation in `Frame::resume` (0 when tier-2 code is
  installed for the code object, so the general path moves the activation to tier 2);
  `genobject::gen_send_fast` (used by Send/yield from, ForIter via tp_iternext, send())
  enters it through a process-wide trampoline (`jit::trampoline`/`jit::enter`): push
  the sent value, link exc_info/frame, recursion check, call, unlink. Deopt/error
  continue in `eval_inner` (`jit::continue_in_interp`). Created frames get their entry
  in `new_generator_for` (`jit::generator_entry`, which also counts the creation
  towards tier-2 compilation). `gen_send_ex` (throw/close/tracing) resets `resume`.
  `RPYTHON_JIT_GENFAST=0` disables it.
* Generator-function calls (exact positional) build the generator frame directly in
  `call_function`; finished generators skip the finalizer dance in dealloc.

### Tier 2
* Generator/coroutine bodies (standard unit only): `build::resume_points` = the
  instruction after every YieldValue; YieldValue ends a CFG block, each resume point
  gets a synthetic entry block (like an OSR entry: stack in frame, unboxed locals
  reloaded) and an entry in `OptCode::entries`. `Term::Yield` stores the result,
  syncs dirty locals, writes lasti/next_instr/yield_from_send/stack_top and the address
  of the tier-2 resume entry into `Frame::resume`, and exits. Locals stay boxed in
  generator units (boxing every unboxed local at each yield and unboxing at each resume
  cost more than it saved). No OSR units for generators. `RPYTHON_JIT2_GEN=0` disables.
  `jit::opt_entry` looks resume pcs up in the standard unit.
* Inlining: callees with forward branches (paths duplicated up to 8 paths/160
  instructions, each return moves its result into one vreg and jumps to the
  continuation; the caller's state after the call is its state before it), nested
  inlining up to 3 levels (an exception in an inner callee raises in the rebuilt
  enclosing callee frame: InlineError chain), LoadMethod in callees, constructor
  calls of plain classes (`try_inline_new`: class tag guard, `h2_alloc_instance`,
  `__init__` inlined as a method call, result reference kept on the stack below the
  call so error exits release it, `h2_init_done` checks for None).
* ToBool/UnaryNot of any object (inline True/False/None, `h2_truth` otherwise).
* UnpackSequence of sites that see tuples and lists.
* Correctness (jit3 gap): any emitted decref that may run Python code (__del__,
  weakref callbacks) drops version-tag facts and heap exact-type facts
  (`B::emit` -> `forget_tags`; int/float/str/bool decrefs exempt), as do truth calls.
  Known remaining exposure: generic attribute slow paths (`h2_load_attr`) can run
  arbitrary code without dropping facts (pre-existing).
* `RPYTHON_JIT_DEBUG=3` logs inlining decisions (not a candidate / rolled back at op).

### Testing
* tools/jit_tests: new t_misc.py (handlers, with, except*, match, imports, closures,
  class bodies, async/await, async for/with, async generators, throw into yield from);
  t_inline.py extended (branchy, nested, constructors, __init__ errors/non-None,
  finalizer mutating a class); t_iter.py (mixed tuple/list unpacking). All pass in all
  six configurations.
* test_asyncio.test_free_threading crashes intermittently in master too (with the JIT
  off: 2/2 runs of the `core` build crashed); with jit4's tier 1 it crashes reliably
  (decref of a bad pointer in a multi-threaded event loop). Not JIT-specific; reported.

## Missing / next steps (previous jit workstream)
* Tier 2 (optimising JIT): SSA from bytecode plus profiles, type speculation with guards
  and deopt by materialising the frame (the frame layout is already shared), unboxed
  int/float in loops, inlining. Not started.
* Tier 1: refcount elimination for LoadFast operands (borrowed stack entries), inline
  Python-to-Python calls (frame setup in machine code), helpers for the remaining
  opcodes so fewer deopts happen (with, raise/except, LoadSuperAttr inline).
* Calls: Frame (Box + Vec slots, pool) setup/teardown is ~150 instructions; it should be
  a contiguous per-thread frame stack.
* Inline instance values (coordinator assignment, not started). Attributes would live
  in a values array keyed by a per-type shared "shape", and a real dict would be
  materialised only for `__dict__` or when the shape is broken. This touches generic
  getattr/setattr, `dict_slot` users, subtype traverse/dealloc and the C API
  (PyObject_GetDict, _PyObject_GetDictPtr). Small dicts are already allocation-cheap
  (see above).
* Startup: 49M vs 40.5M. The remaining cost is executing importlib for ~20 modules and
  class creation; the next step is caching module code or avoiding Python-level
  importlib for builtin/frozen modules.
* Thread safety caveats: cached borrowed pointers (cache values, dict entries read by
  JIT code) rely on deferred reclamation at safepoints (D6), which the containers do not
  implement fully yet. This is the same exposure as their own lock-free readers.
