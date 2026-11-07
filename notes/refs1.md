# refs1 — borrowed value-stack references

Branch: the refs1 worktree (base: core 282b096). Everything written from scratch.
Goal: locals pushed on the value stack without reference-count traffic where that is
safe (HotSpot keeps locals and operands in frames/registers; CPython 3.14+ has
LOAD_FAST_BORROW). Measured cause (scale1/scale2): every LOAD_FAST incref'd and the
consumer decref'd, ~10% of single-thread instructions, and with threads every core
wrote the refcount line of shared objects (shr_dict, shr_attrs, mut_list, mut_dict).

## Decision proposal (for DECISIONS.md, coordinator; the brief said D30, which is taken)

**D35 — Borrowed value-stack references.** A frame's value-stack slot may hold a
borrowed reference to a local's object, marked by pointer bit 0 (only value-stack slots,
never locals or anything C code sees). The runtime's translator decides which loads
borrow (`LoadFastBorrow`, its own analysis: every copy consumed in the same basic block,
no store to the local in between, no handler keeping it); the interpreter and both JIT
tiers accept tagged entries wherever they consume stack values (tier 1 statically,
per-instruction masks; tier 2 passes borrowed template inputs tagged). Writes to a
running frame's locals from outside its code defer the release of the old value to the
frame's end. `sys.getrefcount` reports real counts for borrowed locals (D27's
caller-stack emulation is retired; only the argument-moving part remains).
`RPYTHON_BORROW=0` disables it.

## Design

**Representation.** A value-stack slot (`Frame::slots[nlocalsplus..]`) may hold a
*borrowed* reference: the object pointer with bit 0 set (`interp/borrow.rs`, `TAG`).
Only value-stack slots are ever tagged — locals, cells, containers, C code never see a
tag. A borrowed slot owns nothing: popping it does nothing; a consumer that keeps the
value (store into a local/container/attribute, return, yield, move into a callee frame)
converts it to an owned reference (`borrow::own`). NULL is never tagged.

**Producer: `LoadFastBorrow`** (internal opcode, reuses the former `Unused2` slot of
`rpy_compiler::bytecode::Op`; never emitted by the compiler). The runtime's translator
(public wordcode → internal code, `interp/translate.rs`) runs `borrow::mark` on the
internal code: one forward pass that follows every `LoadFast` until all copies of its
value are consumed (LoadMethod/LoadSpecial/LoadSuperAttr-method pass the receiver on as
`self`: tracked as a copy). A load becomes `LoadFastBorrow` only if
* every copy is consumed inside the same basic block (no jump/branch/return/raise/yield
  and no jump target or handler entry before that),
* no instruction of the frame rebinds or deletes the local while a copy is live
  (StoreFast/DeleteFast/LoadFastAndClear; `x = x` is fine: the store owns first),
* no exception handler covering an instruction would keep a copy on the stack
  (handler depth > the copy's stack position),
* every instruction on the way is in the analysis' model (others: not borrowed).
Consequences: no borrowed entry crosses a block boundary, a yield or a suspension, so
compiled code (entered only at block starts: function entry, loop headers, handlers,
resumption) never finds one at entry, and suspended generator frames never hold one.
The public bytecode's own LOAD_FAST_BORROW marks are not trusted (CodeType/pyc input):
the runtime decides with its own analysis. Cost: ~1.4M instructions of `-c pass`
startup (65.2M → 66.8M, +2.5%), one pass per translated code object.

**Writes from outside the frame.** The `f_locals` proxy (and anything else that rebinds
a running or suspended frame's local) parks the old value in `Frame::keep`
(`borrow::keep_alive`) instead of releasing it; the frame releases it with itself
(`Frame::release`; the machine-code call return path takes the general release when
`keep` is set). This also fixes a pre-existing tier-2 use-after-free: tier 2 keeps
borrowed copies of locals in registers across calls, and an f_locals write freed the
object under them (tools/jit_tests/t_borrow.py crashed the base build at
RPYTHON_JIT2_THRESHOLD=1). A debugger jump (`f_lineno`) converts the stack entries it
keeps to owned ones (`borrow::own_stack`), since the target code was not analysed with
them.

**Interpreter.** `pop!` returns the raw slot; `popref!`/`popopt!` convert to owned
(every untouched consumer stays correct); `popsr!` gives an `SRef` (deref to the object,
drop releases only an owned entry) used by the consumers that only look at their
operands (LoadAttr, StoreAttr's object and value, the binary/compare/contains/is/subscr
operators, unary ops, ToBool, the branches, GetIter, DeleteAttr/DeleteSubscr);
`top!`/`peek!` untag. LoadMethod keeps a borrowed receiver borrowed as `self`
(`specialize::load_method_r`: "self is the receiver" instead of a new reference).
Calls pass C callees an untagged copy of the argument block (scalar copy into a
16-entry stack buffer; more than 16 entries are owned in place) and release the stack
entries by tag afterwards; the frame's slots stay tagged so frame/generator traversal
and unwinding keep skipping them. Unwinding, frame release/clear/recycle and
`gen_traverse`/`frame_traverse` skip tagged slots.

**Helpers** (`jit/helpers.rs`): `take` converts to owned (default), `take_sr`/`view` for
the look-only helpers; `h_call` (no tags possible) and `h_call_t` (copies) share
`call_impl::<TAGS>`; `h_prof` untags; `h_call_py` and the build helpers own what they
move.

**Tier 1.** Static and exact: `tag_masks` computes, per instruction, which stack depths
hold borrowed entries (LoadFastBorrow pushes one, LoadMethod passes a borrowed receiver
on, Swap moves them, everything else consumes/pushes owned). Templates that accept them
(`tag_aware`: PopTop, StoreFast, Copy, Return, IsOp, the branches, ToBool/UnaryNot,
LoadAttr/LoadMethod/StoreAttr inline caches, BinaryOp, CompareOp (+fused branch),
BinarySubscr, the calls and the helper-only templates) remove the tag before using an
object (`load_obj`) and skip the release (`release_obj`); every other template first
owns all borrowed entries (`own_all`). Calls: the generic path uses `h_call_t` when an
argument may be borrowed; the Python-function path takes the callee's references inline
(dynamic tag test: a statically borrowed `self` slot may hold NULL or an unpacked owned
self) and the property-getter path takes a reference for `self`, so the machine-code
call routine (`callpy.rs`) only ever moves owned arguments. Around a generic call
(`h_call`/`h_call_t`) the template sets the frame's `stack_top` to the call's top and
restores it (compiled code does not maintain it; D27's argument-moving rule reads the
caller's operands). `owned_loads`: call sites that the interpreter saw calling a Python function
(specialisation view) get their argument loads emitted as owned loads — tier 1 moves
those arguments into the callee frame, and a reference taken at the load is cheaper
than converting at the call.

**Tier 2.** Its abstract stack already had borrowed local values (`Own::Borrowed`);
they were materialised with an incref before every template. Now the inputs a template
consumes are stored tagged instead (`materialize_for_template`; `Inst::Template` carries
the mask, the embedded tier-1 generator uses it). Safe without the translator's proof:
the template consumes them in that instruction (an exception unwinds them, a deopt
re-executes the instruction in the interpreter, which accepts tags). LoadMethod
templates and Python-function call sites keep owned inputs. Deopt snapshots and joins
keep owned entries (the interpreter could otherwise meet a tier-2 borrow the
translator did not prove).

**GC.** Frames' value stacks were never roots (only locals); `frame_traverse` and
`gen_traverse` skip tagged slots, so a borrowed copy is not counted as a reference.

**sys.getrefcount (D27).** With borrowed loads the count of a local argument is the
real one (CPython 3.14+ semantics): the "caller's own stack copy" emulation is gone
(kept only for RPYTHON_BORROW=0). The second part stays: our calls copy arguments
into Python callees where CPython moves them, so while the parent frame is inside the
call with an *owned* operand of the object, one counted reference is that slot.
`PyUnstable_Object_IsUniquelyReferenced`'s emulation counts owned stack copies only
(tagged ones never compare equal), so it is correct in both modes.

`RPYTHON_BORROW=0` turns the translation step off (no tagged slot is ever created).

## Results

Before = core ad289e5 built from the same toolchain (`target/refs1/coretarget`), after =
this branch (3199042). Callgrind instructions; micro: `(I(60k) - I(20k)) / 40k` per loop
iteration of `tools/bench/micro/<name>.py` (PYTHONDONTWRITEBYTECODE=1: pyc rewrites by
another binary distort single runs); perloop: `tools/bench/perloop.sh` (M per loop).

| micro (per iteration) | interp before | after | tier 1 before | after | default before | after |
|---|---|---|---|---|---|---|
| attr | 2558 | 2468 (-3.5%) | 475.5 | 417.1 (-12.3%) | 313.4 | 311.0 |
| method | 3734 | 3686 (-1.3%) | 636.4 | 613.5 (-3.6%) | 284.1 | 285.1 |
| listdict | 3244 | 3141 (-3.2%) | 1120.2 | 1077.1 (-3.8%) | 995.7 | 975.4 (-2.0%) |
| loop | 1702 | 1630 (-4.2%) | 389.1 | 330.3 (-15.1%) | 45.8 | 47.5 |
| globals_ | 3969 | 4002 (+0.8%) | 1090.7 | 1074.8 (-1.5%) | 161.3 | 159.6 |
| calls:posmany | 2160 | 2148 | 481.2 | 488.3 (+1.5%) | 110.0 | 112.2 |
| calls:bound | 1863 | 1868 | 423.5 | 441.0 (+4.1%) | 451.5 | 461.3 (+2.2%) |
| calls:builtin | 4089 | 4143 (+1.3%) | 1628.8 | 1687.8 (+3.6%) | 1332.9 | 1374.5 (+3.1%) |
| calls:prop | 2041 | 2011 (-1.5%) | 372.3 | 366.6 (-1.5%) | 397.1 | 405.6 (+2.1%) |
| calls:kw | 1988 | 2023 (+1.8%) | 767.9 | 793.7 (+3.4%) | 759.7 | 793.4 (+4.4%) |
| calls:init | 3195 | 3194 | 1449.5 | 1471.9 (+1.5%) | 756.8 | 758.8 |

perloop (M instructions per loop, default tiers): richards 268.45 → 269.28, nbody 367.59 →
366.82, float 566.78 → 572.24 (+1.0%), chaos 355.33 → 358.19 (+0.8%), raytrace 2380.35 →
2394.57 (+0.6%), deltablue 40.44 → 40.39, go 1070.40 → 1065.37, spectral_norm 679.97 →
680.39. Tier 2 already kept locals borrowed inside its units, so single-thread tier-2 code
gains little; the remaining C-call costs (`h_call_t` copies the argument block, ~25
instructions more than `h_call` for 3-4 entries, against ~12 saved per borrowed argument)
show up in call-heavy code.

Start-up (`-c pass`): 57.88M → 59.27M (+2.4%; RPYTHON_BORROW=0: 58.13M): the marking pass
costs ~1.3M (234 code objects, ~80 instructions per word: about 25% of the translator's
own cost). Frozen modules come pre-marked from the start-up image.

Multi-core (`tools/bench/scaling/run.sh`, `THREADS="1 4" REPS=3`, best runs, machine load
4-6; 4-thread runs did not pass the contention filter, so cpu-infl is the meaningful
column):

| workload | cpu-infl@4 before | after | wall@1 before | after |
|---|---|---|---|---|
| shr_dict | 1.48 | **1.02** | 0.333 s | 0.256 s |
| shr_attrs | 1.78 | **1.11** | 0.086 s | 0.087 s |
| mut_list | 2.38 | **1.25** | 0.146 s | 0.111 s |
| mut_dict | 1.11 | 1.12 | 0.405 s | 0.266 s |
| cpu_comp | 1.33 | **1.07** | 0.251 s | 0.255 s |
| cpu_oo | 1.09 | 1.02 | 0.135 s | 0.138 s |
| jit_calls | 1.04 | 1.02 | 0.129 s | 0.132 s |

Non-owner refcount operations on runtime paths (`RPY_SCALESTATS=1`, 4 threads, scale
0.3; JIT-inline operations are not counted): shr_dict decrefs 22.8M → 8.4M (the shared
dict: 8.4M → 0), mut_list 4.43M → 2.22M (the shared list: 2.16M → 0), mut_dict 12.35M →
0.05M (the shared dict: 12.29M → 0); shr_attrs 6.35M and cpu_comp unchanged (attribute
values loaded from the shared instance, owned `self` of Python callees).

Correctness: tools/jit_tests/run.sh (all 31 scripts, incl. the new t_borrow.py) and
tools/stress/run.sh (repeatedly) pass; the suite gate (test_sys test_capi test_gc
test_weakref test_finalization test_generators test_coroutines test_asyncgen
test_sys_settrace test_monitoring test_frame test_exceptions test_traceback
test_threading test_free_threading test_descr test_class test_call test_dict test_list
test_pdb test_inspect test_scope test_with test_contextlib) on installed-layout snapshots
fails exactly what the base snapshot fails (test_descr 2, test_gc 2, test_sys_settrace
1). test_threading's test_reinit_tls_after_fork hung once in some runs (2 of ~40 runs on
this branch, 0 of 22 on base builds): the forked child deadlocks in
`modules/_thread.rs` `handle_set_done` on a handle's `std::sync::Mutex` that another
thread held at fork time (backtrace taken from the hung child) — a pre-existing
fork-safety bug outside this branch's paths, made more or less likely by timing.

## What is left
* Tier 2 keeps owned references for folded constants (`len`, method descriptors) passed
  to templates: borrowing them needs a guarantee that the dict/type keeping them alive
  cannot drop them during the call (deferred freeing, like sharded objects).
* Tier-2 code does not see an `f_locals` write to a boxed local whose value it keeps in
  a register (pre-existing; no longer a use-after-free thanks to `keep_alive`).
* Callee-side borrowing (arguments of Python calls stay borrowed in the callee frame)
  would need tagged locals.
* C calls with borrowed arguments copy the argument block (`h_call_t`, the interpreter's
  call): 2-4% more instructions on C-call micro benchmarks in tier 1/2. A template could
  build the untagged block in the native frame with statically known entries, or pass a
  borrowed-entry mask so the block can be untagged in place (the frame/generator
  traversal would then need that mask during the call).
* Start-up: marking lazily (when a code object runs a second time) instead of at
  translation would remove the ~1.3M instructions of `-c pass`; it needs the code words
  rewritten while other threads may run them (each word stays valid on its own).
* Tier-2 deopt snapshots and joins still materialise borrowed values with a reference
  (they would need the translator's proof per entry).
