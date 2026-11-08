# perf8 workstream: JIT code size, shared call routines, deopt storm in logging

Branch: the perf8 worktree (based on `core` b9124d2 + perf7 merged; `core` merged again at
the end, b23a559). Everything written from scratch. Method as perf3-perf7: callgrind and
cachegrind counts decide (tools/bench/perloop.sh, cachestat.sh, micro/percall.sh), wall
clock is indicative only (load 7-9 from other agents during this work).

New tools (scratch scripts were used for the analysis; the in-tree ones):
* `RPYTHON_JIT_OPSTATS=1`: running totals of tier-1 code bytes per bytecode kind (inline
  and out-of-line stubs) and of tier-2 bytes per IR instruction kind / template, printed
  after every compilation (the last table covers the process).
* `tools/bench/micro/genx.py` (generator expressions consumed by builtins, zip loops).
* jit tests: `t_mixednum.py`, `t_dictattrs.py`, `t_ccalls.py`.

## Findings

* **logging deopt storm (the largest single cause of logging's gap).** Tier 2 converted an
  int operand of a mixed int/float operation (`UnboxNumF`) only when it had one 30-bit
  digit, and deoptimised otherwise. `LogRecord.__init__` computes
  `(stamp - _startTime) / 1e6` with nanosecond ints: once the process has run for
  1.07 s the difference has two digits, every call deoptimised, after 64 the tier-2 unit
  was discarded, recompiled, discarded again, and `LogRecord.__init__` ran in tier 1 (or
  the interpreter) for the rest of the run. Short callgrind runs never saw it; any
  pyperformance-length run does. Two-digit ints (|v| < 2^60, exact in i64, converted
  with cvtsi2sd = correctly rounded) are now converted inline.
* **Instance attributes of objects whose `__dict__` was materialised** went to the generic
  `specialize::load_attr` from tier 2 on every access (logging: 342 calls per loop,
  `record.__dict__` in Formatter materialises every record's dict; handlers and loggers
  too). Tier 2 now reads them at the cached index of the dict (as tier 1 did), with the
  shared_read rule for lock-free dict reads when threads run.
* JIT code was ~270 bytes per bytecode in both tiers; executed JIT bytes of logging ~60 KB
  per loop plus ~100 KB of runtime code (L1i 64 KB). Tier-1 inline bytes per kind
  (logging, before): LoadGlobal 379, StoreAttr 527, BinaryOp 389, LoadAttr/LoadMethod
  ~220, Call ~250; in tier 2, more than half the bytes were template stubs and routines,
  a quarter snapshot stubs.

## Changes

### Code cache near the executable (mem.rs, x64.rs)
HotSpot reserves its code cache once; here the first chunk of executable memory is a
256 MB reservation (sparse memfd, only written pages cost memory) whose executable view is
mapped within +-1 GB of the executable's text (hints below the executable, then above; a
mapping placed elsewhere is given back). `mem::near_window()` tells the assembler which
targets every byte of the reservation reaches with rel32; `call_abs`/`jmp_abs` then emit
`call/jmp rel32` (5 bytes instead of `movabs rax, imm64; call rax`, 12) and `mov_ri` of a
64-bit value in reach emits RIP-relative `lea` (7 bytes instead of 10). The fixups
(`Asm::rel_abs`) are applied at installation (`mem::install_asm`); `install` refuses code
whose rel32 targets do not reach from where it would land (reservation full: the JIT
stops compiling, as HotSpot's does when its code cache is full). Every installation site
goes through `install_asm`. Fork copies only the used part of the reservation.
`RPYTHON_JIT_FAR=1` disables the reservation (long forms). Unit test `x64::tests::near_forms`.

### Shared call routines (callpy.rs `dispatch`, tier1.rs Call/CallKw)
A Call site was ~250 bytes (function check, fast-entry checks, bound-method unpacking,
`type(x)`, the generic call with its stack_top bookkeeping and the poll). Now the site is
`lasti = pc; rdi = &slot[base]; esi = argc; call dispatch; test; jne error` (~35 bytes)
and four process-wide routines (plain/borrowed entries x Call/CallKw) do the rest:
* Python function with a published fast entry: tail-jump to jit6's call routine (callpy);
* bound method with a free self slot: unpack, retry; `type(x)`;
* builtin functions and method descriptors (exact type for the descriptor's self) with
  METH_O / METH_NOARGS / METH_FASTCALL (with and without keywords): their C function is
  called from the routine, the entries released inline (borrowed ones passed untagged,
  copied to the native stack for FASTCALL, and not released: callees see the true
  reference counts, as through h_call_t);
* everything else: `h_call`/`h_call_t` with stack_top = the call's top, then the
  eval-breaker poll (a poll error releases the result: the error exit's depth excludes it).
`RPYTHON_JIT_SHAREDCALL=0` emits the old inline sequences.

### Tier-1 code size
* LoadGlobal: one shared lookup routine (`load_global_routine`: entry in rcx, value or 0
  in rax), the site keeps the call, the store and an out-of-line slow path (379 -> ~30
  inline bytes).
* incref/decref: memory-operand forms (`cmp dword [r+rc], -1; ...; add/sub dword [r+rc], 1`):
  19/22 bytes instead of 23/30, r11 no longer clobbered (`emit_incref_fast`,
  `emit_decref_fast`, shared with tier 2).
* int + - * accept bool operands inline (`c += flag`); the bool is profiled as OTHER so
  tier 2 does not unbox it as an int (it would re-box True as 1).

### Tier-2 code layout and size (codegen.rs, build.rs, regalloc.rs)
* Block layout (`layout`): synthetic blocks were emitted after all bytecode blocks, so the
  straight-line path of an attribute store jumped to the end of the unit and back. Blocks
  are now chained (each block's Jump target or the second target of a Branch, i.e. the
  continuation of a `check`, right after it) and cold blocks (`Block::cold`: generic
  helper paths, multi-threaded variants, deoptimising branches, and synthetic blocks
  reached only from cold ones) go after all hot blocks. Register allocation keeps its own
  order (every value has one location). `RPYTHON_JIT2_LAYOUT=0` keeps creation order.
* A Branch whose true target falls through is one inverted jcc for integer conditions.
* Decref slow paths: one per-register entry per unit (`dec_entries`) instead of a
  push/mov/call/pop copy at every site.
* Instance attributes from materialised dicts (above).
* Register allocation: no allocations per block per liveness pass or per vreg in the
  scan, liveness iterated in the IR CFG's postorder (deltablue 1/3-loop window 44.9M ->
  41.7M instructions: compile time).

### Runtime
* List iterators step without the lock when one thread runs (zip/map/enumerate over
  lists).
* `h_call` stays a separate (untagged) instantiation (a tag-aware single copy cost
  coroutines 1%).

## Measurements
"base" = start of perf8 (core b9124d2 with perf7), normal build; "m3" = branch before the
final core merge; "core" = `core` b23a559 as built by the core agent (no perf8);
"final" = branch head after the merge, normal build; "pgo" = final through
tools/pgo/build.sh (profile trained on the merged head before the last one-line fix,
reused with PGO_PROFILE); CPython = /opt/baseline/py-default (perf7's numbers, same
machine and scripts).

### JIT code size (RPYTHON_JIT_DEBUG totals, 300 loops)
| bench | tier 1 bytes (B/bytecode) base -> final | tier 2 bytes (B/bytecode) base -> final |
|---|---|---|
| logging | 499,648 (267) -> 285,660 (153) | 365,634 (266) -> 288,176 (209) |
| deepcopy | 219,169 -> 102,871 | 221,162 (285) -> 158,934 (205) |
| typing_runtime_protocols | 281,109 (283) -> 119,807 (121) | 108,405 (201) -> 68,756 (127) |
Executed JIT bytes per logging loop (callgrind, hot lines): 87 KB -> 60 KB (m3).

### Per benchmark loop, cachegrind (instructions / L1i misses; tools/bench/cachestat.sh)
| bench (window) | base | m3 | core | final | pgo | perf7 pgo | CPython |
|---|---|---|---|---|---|---|---|
| typing_runtime_protocols (300/1300) | 1.87M / 24.9K | 1.50M / 14.6K | - | 1.82M / 17.8K | 1.47M / 7.6K | 1.55M / 11.6K | 1.69M / 3.6K |
| typing (1300/2300) | - | - | 1.77M / 23.6K | 1.74M / 17.1K | - | - | - |
| deepcopy (100/400) | 3.31M / 65.2K | 3.14M / 55.2K | 3.34M / 63.5K | 3.22M / 58.4K | 2.78M / 53.9K | 2.80M / 59.2K | 2.84M / 12.4K |
| logging (400/1000) | 1.31M / 52.8K | 1.05M / 44.1K | 1.36M / 54.7K | 1.23M / 47.5K | 1.13M / 42.7K | 1.32M / 47.9K | 1.05M / 11.5K |
The final core merge took back most of m3's instruction gain on typing and part of it on
logging (core alone measures 1.77M / 1.36M; the cause is in what the merge brought, not
identified: no deopts, no recompilation, GC < 2%); against core the branch is -1% / -10%
instructions and -28% / -13% L1i misses on typing / logging (normal builds).

### Instructions per operation (micro/percall.sh)
| kind | base | final | CPython |
|---|---|---|---|
| calls.py builtin (`isinstance`, `id(l)`, `l.copy()`) | 1372 | 1272 (m3 1234) | - |
| calls.py getattr_call / init / bound / kw | 1184 / 758 / 464 / 792 | (m3) 1145 / 740 / 461 / 798 | 1094 / 1253 / 502 / 701 |
| comp.py enumcmp (`c += w.kind == Kind.BIG`) | 708 | 399 | 649 |
| comp.py anygen | 4314 | 3999 (m3) | 3464 |
| genx.py allzip / sumgen / tuplegen / zipiter | 8876 / 3500 / 4095 / 5333 (m) | 8669 / 3499 / 4108 / 5129 (m) | 5626 / 3773 / 3726 / 3925 |

### Steady state per loop (perloop.sh 1/3, millions; base vs m1 = before LoadGlobal routine)
richards 269.4 -> 260.0, float 572.3 -> 549.4, chaos 358.0 -> 320.3, nqueens 890.9 ->
886.6, raytrace 2394.6 -> 2322.1, hexiom 62.6 -> 61.2, coroutines 289.4 -> 291.3 (after
reverting the shared h_call instantiation), spectral_norm 680.5 -> 650.8, fannkuch 3561 ->
3486, nbody 367.0 -> 342.7, async_generators 3321 -> 2972, xml_etree 4083 -> 3978, pprint
12000 -> 11547, regex_v8 221.9 -> 220.0; json_dumps (3/9) 83.9 -> 77.6; deltablue (4/8)
26.57 -> 25.85 (CPython 29.1), deltablue 1/3 window 43.0 -> 41.7 (compile time: the
"+10% since perf5" is compilation inside the short window, steady state is below perf5).
go 1047 -> 1045, generators 292.4 -> 290.6.

### Wall clock, PGO build vs the best CPython build (s per loop)
tools/bench shim, ours / CPython default / CPython free-threaded back to back per
benchmark, pinned to core 3, load 10-13 from other agents: one full pass, and two more
passes for the benchmarks behind in the first; minimum of the passes. Indicative only
(single values move by 10-30% at this load). ratio = ours / min(default, ft).

| bench | ours | default | ft | ratio |
|---|---|---|---|---|
| decimal_pi | 0.2147 | 9.318 | 9.883 | 0.02 |
| telco | 0.008989 | 0.1833 | 0.1976 | 0.05 |
| regex_compile | 0.02251 | 0.1427 | 0.1769 | 0.16 |
| pidigits | 0.1139 | 0.3043 | 0.3193 | 0.37 |
| regex_effbot | 0.001563 | 0.003577 | 0.003508 | 0.45 |
| gc_collect | 0.0005359 | 0.001345 | 0.00114 | 0.47 |
| regex_dna | 0.1308 | 0.2502 | 0.2624 | 0.52 |
| richards_super | 0.04018 | 0.07052 | 0.08567 | 0.57 |
| richards | 0.03677 | 0.06149 | 0.0572 | 0.64 |
| spectral_norm | 0.08194 | 0.1255 | 0.1522 | 0.65 |
| float | 0.0391 | 0.05926 | 0.06512 | 0.66 |
| deltablue | 0.001811 | 0.002687 | 0.003956 | 0.67 |
| json_dumps | 0.004501 | 0.00676 | 0.009274 | 0.67 |
| chaos | 0.02594 | 0.03797 | 0.06332 | 0.68 |
| nbody | 0.06734 | 0.1136 | 0.0995 | 0.68 |
| concurrent_imap | 0.0397 | 0.05645 | 0.05381 | 0.74 |
| regex_v8 | 0.02292 | 0.0322 | 0.0308 | 0.74 |
| gc_traversal | 0.00107 | 0.003729 | 0.001424 | 0.75 |
| barnes_hut | 0.804 | 1.061 | 1.203 | 0.76 |
| scimark | 0.2025 | 0.2661 | 0.3507 | 0.76 |
| pprint | 0.6501 | 0.8092 | 0.8683 | 0.80 |
| json_loads | 1.722e-05 | 1.987e-05 | 1.934e-05 | 0.89 |
| bpe_tokeniser | 4.244 | 5.27 | 4.705 | 0.90 |
| unpack_sequence | 3.903e-08 | 4.33e-08 | 5.863e-08 | 0.90 |
| async_generators | 0.2804 | 0.3003 | 0.3795 | 0.93 |
| raytrace | 0.3469 | 0.3743 | 0.4124 | 0.93 |
| fannkuch | 0.323 | 0.3379 | 0.4005 | 0.96 |
| go | 0.08499 | 0.08714 | 0.09323 | 0.98 |
| python_startup | 0.01185 | 0.0156 | 0.01194 | 0.99 |
| xml_etree | 0.1726 | 0.1704 | 0.1722 | 1.01 |
| logging | 5.04e-06 | 4.78e-06 | 5.263e-06 | 1.05 |
| generators | 0.02451 | 0.02303 | 0.02481 | 1.06 |
| sqlite_synth | 2.09e-06 | 2.566e-06 | 1.871e-06 | 1.12 |
| hexiom | 0.004603 | 0.003937 | 0.004643 | 1.17 |
| pathlib | 0.01346 | 0.01151 | 0.01109 | 1.21 |
| typing_runtime_protocols | 0.0002148 | 0.0001758 | 0.00019 | 1.22 |
| nqueens | 0.09164 | 0.07334 | 0.09737 | 1.25 |
| deepcopy | 0.0002516 | 0.0001977 | 0.0002045 | 1.27 |
| base64 | 0.0002989 | 0.000247 | 0.0002324 | 1.29 |
| meteor_contest | 0.1399 | 0.1042 | 0.1531 | 1.34 |
| coroutines | 0.01963 | 0.01456 | 0.01444 | 1.36 |
| asyncio_tcp | 0.9894 | 0.7606 | 0.6698 | 1.48 |
| mdp | 1.632 | 1.098 | 1.596 | 1.49 |
| comprehensions | 1.784e-05 | 1.061e-05 | 1.151e-05 | 1.68 |
decimal_factorial: CPython builds ERROR (both). argparse, async_tree, crypto_pyaes,
pickle and the third-party benchmarks were not run (packages).

comprehensions, cross-checked (min of 3 interleaved, s/loop): base (normal build)
1.32e-5, core 1.63e-5, final normal 1.57e-5, final pgo 1.34e-5, with identical instruction
counts (perloop 50/300: 0.23M all normal builds, pgo 0.20M, CPython 0.19M): the wall-time
loss came with the core merge and is not in the instruction count. coroutines, mdp,
meteor_contest and asyncio_tcp did not get this round's attention; logging (1.67 in
perf7) and deepcopy (1.31) moved to 1.05 and 1.27.

## Correctness
* tools/jit_tests/run.sh (29 scripts, with the new t_mixednum, t_dictattrs, t_ccalls) and
  tools/stress/run.sh (with the new ccall_poll_race) pass on the final normal build and on
  the PGO build.
* Gate (tools/run_tests.sh on an installed snapshot of the merged head): test_inspect,
  test_sys, test_list, test_call, test_descr, test_gc, test_sys_settrace, test_generators,
  test_coroutines, test_monitoring, test_threading, test_traceback, test_exceptions,
  test_frame, test_pdb, test_capi, test_dict, test_free_threading, test_asyncio,
  test_typing, test_json, test_functools, test_dataclasses, test_opcache, test_copy,
  test_logging, test_xml_etree, test_xml_etree_c, test_enum: 11,053 tests, 0 failures, 0
  errors (core's own known failures do not occur on this snapshot). test_free_threading
  crashed in the gate run: the poll after a direct C call restored stack_top from r14,
  which the release loop had reused as its cursor, and scale3's parallel collector then
  read a running generator's frame past its slots. Fixed (callpy.rs), reproduced and
  verified with tools/stress/ccall_poll_race.py (2/3 aborts before, 0/5 after);
  test_free_threading afterwards: 15 of 16 runs pass, one run reported a failure whose
  test I could not capture in later runs (perf5/perf7 noted rare flakes in this package:
  test_mmap's BufferError, a test_json race); core's binary passed 4 of 4.
* crates/launcher/tests/startup_order.rs passes after the merge (startup.ld unchanged).
* x64 unit test `near_forms`.

## What is left (measured causes)
* L1i misses are still 2-5x CPython's with PGO (typing 7.6K vs 3.6K, deepcopy 54K vs 12K,
  logging 43K vs 12K). The runtime's hot code is now the larger part (logging: ~100 KB of
  runtime lines vs 60 KB of JIT lines per loop): allocation, dict probes, `h_call` for
  types and other callables, the interpreter for code that is not compiled.
* Tier-2 code is still ~200 bytes per bytecode: half of it template stubs (cold) and
  snapshot stubs; embedded tier-1 templates (LoadMethod of non-METHOD kinds 208 B,
  BinaryOp 364 B, BinarySubscr 396 B) are the hot part. Next steps: tier-1 attr_op and
  BinaryOp through shared routines like LoadGlobal, error stubs through a per-unit tail
  (25 -> ~8 bytes each), tier-2 LoadMethod of module functions / dict-backed instances.
* Generic calls of types (weakref.ref: 500 per typing loop, exceptions, zip), method-
  wrappers and Python functions without a fast entry (*args, coroutines) still go
  through `h_call` (~100 instructions of its own).
* Generator expressions / zip: per item the chain h_for_iter -> iter_next -> zip_next ->
  next_items_buf -> list iterator costs ~3x CPython's (allzip 8.7K vs 5.6K).
* asyncio_tcp, comprehensions, base64, pathlib, mdp, meteor_contest, scimark, xml_etree
  remain behind in wall time (see the table); not investigated in this round.
* The typing / logging instruction counts lost in the final core merge (see above).
