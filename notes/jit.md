# jit workstream — execution tiers, inline caches, allocator/GC/startup performance

Branch: `worktree-agent-adf8b04d272001116`. Everything here was designed and written
from scratch (no codegen crates, no reference to other implementations' source).

## Architecture (HotSpot-style tiers, DECISIONS D7)

| tier | where | what |
|---|---|---|
| 0 | `interp/eval.rs` (+ `interp/specialize.rs`, `interp/cache.rs`) | the interpreter; it is the reference semantics |
| 1 | `interp/jit/` | baseline "template" JIT for x86-64: every bytecode turns into a fixed machine-code sequence |
| 2 | — | not done (see "Missing") |

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

### Status / known problems
* **Tier 2 is opt-in** (`RPYTHON_JIT2=1` or `RPYTHON_JIT2_THRESHOLD=n`): with it on,
  the spec smoke segfaults in test_generators (test_bool/test_set/test_list/test_dict
  results unchanged). Not yet debugged. Note: test_generators and test_descr also
  segfault with the pre-jit2 binary and with RPYTHON_JIT=0 (pre-existing, not JIT).
* Folded globals are invalidated on *any* write to the module dict (no per-key
  dependency yet): module-level loops that assign globals cause invalidation storms
  (then folding is switched off for that code). Fix: pass the key to
  `cache::on_dict_write` and keep (dict, key) dependencies.
* Unboxed locals are synced lazily: arbitrary code reached from an escaping
  instruction (e.g. `sys._getframe(1).f_locals`) sees stale values of unboxed locals
  of a running tier-2 frame (exits, deopts and exceptions do sync).
* No measurements of tier 2 on pyperformance yet (it was finished at the pause).

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

### Next steps (in order)
1. Debug the tier-2 crash in test_generators (run it with `RPYTHON_JIT2=1`), then
   enable tier 2 by default and measure.
2. Per-key global dependencies; peephole Branch(Test(SetCond c)) -> Branch(c), setcc
   for SetCond.
3. Inline LoadAttr/StoreAttr/LoadMethod in tier 2 with type-tag guards reused across a
   region; inlining of small leaf callees (eval_A-style), virtual tuples (BuildTuple +
   UnpackSequence, enumerate) — scalar replacement.
4. Generators in tier 2 (resume points as entries); frame setup in machine code for
   tier-2 -> tier-2 calls.

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
