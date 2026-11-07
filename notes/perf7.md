# perf7 workstream: why "fewer instructions but slower", profile-guided build, JIT code size

Branch: the perf7 worktree (based on `core` 4116887 = perf6 merged; `core` merged again at
043ee67 with refs1's borrowed stack references, capi3, cleanroom4). Everything written from
scratch. Method as perf3-perf6: callgrind/cachegrind counts decide, pinned wall runs confirm
(D22: other agents running, the same binary varies 10-15% between runs).

## The finding: instruction-cache misses, not instructions

The VM has no hardware counters (`perf_event_open` gives only software events), so the new
`tools/bench/cachestat.sh` runs cachegrind with cache and branch simulation (64 KB 16-way
L1i, 48 KB L1d, as detected) on two loop counts and reports the per-loop difference, like
perloop.sh. It works for the baselines too (it is a tools/bench script).

Per benchmark loop (base = start of perf7):

| bench (window) | Ir ours / CPython | L1i misses ours / CPython | D1 misses | mispredicts |
|---|---|---|---|---|
| typing_runtime_protocols (300/1300) | 1.88M / 1.69M | 30.1K / 3.6K | 2.9K / 2.2K | 25K / 30K |
| deepcopy (100/400) | 3.23M / 2.84M | 64.9K / 12.4K | 4.5K / 1.7K | 43K / 51K |
| logging (100/400) | 1.41M / 1.05M | 55.7K / 11.5K | 18.3K / 11.2K | 26K / 26K |

The instruction counts are within 10-35% of CPython, the L1i misses 5-8x CPython's; branch
mispredictions are not worse. At ~10-15 cycles per L1i miss (L2 hit) the misses alone cost
as much as the whole of CPython's loop. So the remaining wall-clock gap of the call-heavy
benchmarks is code footprint:
* `tools/bench/perf/hotlines.sh` (two callgrind profiles with `--dump-instr=yes`) counts the
  64-byte code lines executed on every loop: deepcopy touches ~180 KB of runtime code and
  ~47 KB of JIT code per loop (L1i: 64 KB). CPython's interpreter loop is compact and shared
  by all Python functions.
* cachegrind attributes 27-42% of the L1i misses to JIT code (logging 42%: tier 2 makes
  106 KB of code for `LogRecord.__init__`, 265 bytecodes, 400 bytes per bytecode; tier 1
  115 KB). A third of the JIT's bytes were jump instructions (rel32 forms).
* The rest is the Rust runtime: generic call paths (h_call ~105 instructions of its own per
  call, vectorcall_raw ~55), allocation, dict, attribute lookup, each a different function
  spread over 10 MB of text, with error handling interleaved with the hot paths.

## Changes

### Profile-guided build (tools/pgo)
`tools/pgo/build.sh` = CPython's `--enable-optimizations` for us: (1) instrumented build
(`-Cprofile-generate`, own target dir, explicit `--target` so RUSTFLAGS - which must repeat
.cargo/config.toml's - do not reach build scripts), (2) installed-layout snapshot +
`tools/pgo/train.sh` (start-up, tools/bench/micro, every pyperformance benchmark without
third-party packages calibrated to ~1 s each, ~55 regression-test modules), (3)
`llvm-profdata merge` (rustup component llvm-tools; installed on this machine), (4)
`-Cprofile-use` build, (5) the start-up order for that executable (below) and a relink.
`PGO_PROFILE=x.profdata` skips 1-3. `tools/install.sh <prefix> pgo` installs it. ~25 min on
this machine (instrumented build 10, training 3, optimized 6, order + relink 6).
Effect (pgo1, 3-loop training; later builds use the calibrated training): typing 1.88M ->
1.71M instructions and 30.1K -> 19.9K L1i misses per loop, deepcopy 3.23M -> 2.66M; wall
(pinned, back to back, ratio to CPython default): typing 1.33 -> 1.07, mdp 1.32 -> 0.97,
nqueens 1.21 -> 1.06, async_generators 1.35 -> 0.90, comprehensions 1.43 -> 1.26,
bpe_tokeniser 1.22 -> 1.08, base64 1.14 -> 1.05, deepcopy 1.20 -> 1.19, logging 1.20 ->
1.38 (noise range; logging's misses are mostly JIT code). Final table below.
Memory cost: the profile inlines more into hot code; peak RSS +0.8 MB (36.6 vs 35.8).
**Decision proposal (D-next):** the benchmark/installed build is the PGO build
(`tools/install.sh <prefix> pgo`), as CPython's release builds are; agents keep using plain
`cargo build --release` for development. Correctness gate run on both.

### JIT code size
* x64.rs: branch relaxation in `resolve` - every jump emitted in the rel32 form (all
  forward jumps) is shortened to rel8 when its final distance allows (monotone iteration
  over candidates, then the code is moved and labels/fixups/positions remapped; backward
  short jumps emitted directly are re-aimed through fixups; `map_pos` translates positions
  taken with `pos()` - tier-1 entries, tier-2 entries). `RPYTHON_JIT_NORELAX=1` disables.
  Unit test `x64::tests::relaxation`. ~8% fewer bytes (most remaining long jumps go to the
  out-of-line stubs, which is where cold code belongs).
* imm8 forms for 32-bit add/sub/cmp (every inline incref/decref: +1/-1), `jcc_short` for
  the fixed-size skips in incref/decref/xincref/xdecref (tier 1 and tier 2).
* (perf7 flags in tier1.rs, opt/codegen.rs, x64.rs, jit/mod.rs.)

### Deopt storm in generator prologues
Tier 2 deoptimises at RETURN_GENERATOR, and pc 0 of generator code is its eager prologue
(a genexpr over a non-builtin iterable, e.g. `all(a is b for a, b in zip(x, y))` in
copy.py): every call deoptimised, after 64 the unit was discarded (with the resumption
entries the generator body needs) and later recompiled, repeatedly. `jit::run` no longer
enters tier 2 at pc 0 of generator code (tier 1 runs the prologue). genexpr micro 9638 ->
9212 instructions per call, no deopts (CPython 5811: zip/genexpr/all overheads remain).

### Runtime
* `isinstance`/`issubclass` call `__instancecheck__`/`__subclasscheck__` unbound
  (`call_unbound`): no bound method per check (typing_runtime_protocols: 130 per loop).
* `list.sort` of exact tuples (comprehensions sorts 4-tuples): equality of items through
  `plain_eq` (exact int/str/float/bytes/tuple) before rich comparison, and the first items
  compared with the kind a pre-scan chose (compact int, bool, str, float); same results as
  tuple `<`. comp.py tuplesort 13683 -> 11918 (CPython 10811).
* Method docstrings (`mdefs!`) live in `.rodata.rpydoc` (`doc_str!`, placed after .rodata
  by crates/launcher/docs.ld): start-up reads method names, which were interleaved with
  the docs (151 KB).

### Start-up order (startup.ld) - decay fixed
* `gen_order.sh` concatenated its callgrind runs sorted on a dot-separated field of a path
  whose temporary directory name contains a dot: cg.10-19 (micro benchmarks) came before
  cg.2-9 (start-up without site, common imports). Runs are now taken in numeric order.
* It runs with `PYTHON_FROZEN_MODULES=on` (an installed python uses the start-up image; a
  source-tree build would read Lib/), keeps only functions of the executable (libc names
  are not matchable), and writes crate-hash-agnostic patterns (`Cs*_` for the `Cs<hash>_`
  of v0-mangled names): the hashes change with flags and builds (the PGO build matched 100
  of 3671 names before), so the list now survives rebuilds; only renamed/re-inlined
  functions drift. `ORDER_STARTUP_ONLY=1` and an output path are optional.
* `crates/launcher/tests/startup_order.rs` (cargo test -p python) fails when more than 10%
  of the listed functions are gone from the executable - the "checked tool step".
* `RPY_ORDER_FILE` lets a build link with another list (the PGO build's own).
* Measured warm (page cache primed; cold measurements vary by ~1 MB): start-up RSS file
  7.9 -> 7.3 MB (anon 3.9), peak 35.5 -> 35.8 MB (noise +-0.3).

### Tools
`tools/bench/cachestat.sh` (cache/branch simulation per loop, baselines allowed),
`tools/bench/perf/hotlines.sh` (code lines executed per loop), `annotate_fn.sh`
(per-instruction costs of one function joined with objdump), `text_pages.py` (resident
pages of each mapping per 256 KB; WORKLOAD=1 runs startup_mem's workload first),
`rss_map.py`, `peak_split.py`, `tools/bench/micro/comp.py` (comprehensions' pieces).

## Measurements
"base" = start of perf7 (4116887, installed snapshot); "m" = branch head built normally
(core merged; snap_m2/m3); "pgo" = branch head through tools/pgo/build.sh (snap_pgo7);
CPython = /opt/baseline/py-default through the bench scripts.

### Per benchmark loop, cachegrind (instructions / L1i misses)
| bench (window) | base | m | pgo | CPython |
|---|---|---|---|---|
| typing_runtime_protocols (300/1300) | 1.88M / 30.1K | 1.85M / 25.5K | 1.55M / 11.6K | 1.69M / 3.6K |
| deepcopy (100/400) | 3.23M / 64.9K | 3.31M / 64.3K | 2.80M / 59.2K | 2.84M / 12.4K |
| logging (100/400) | 1.41M / 55.7K | - | 1.32M / 47.9K | 1.05M / 11.5K |
| nqueens (1/3) | - | 891M / 8.20M | 784M / 1.41M | 845M / 0.36M |
(perloop windows are not always linear for us - JIT compiles fall inside them; the typing
window with RPYTHON_JIT_NORELAX=1 read 1.79M, with relaxation 1.21M.)

### Instructions per operation (`percall.sh`)
| kind | base | head | CPython |
|---|---|---|---|
| comp.py tuplesort (8 tuples built + sorted) | 13683 | 11918 | 10811 |
| comp.py anygen / enumcmp / listcomp / dictcomp / dcinit | 4266 / 861 / 2303 / 5158 / 1996 | 4317 / 843 / 2307 / 5303 / 1993 | 3464 / 649 / 2441 / 4314 / 1779 |
| genexpr `all(a is b for a, b in zip(l, t))` | 9638 | 9212 | 5811 |
| `-c pass` | 39.2M | 39.4M (pgo 34.8M) | 40.5M |

### Wall clock (s/loop ratio to CPython default; min of 3 interleaved runs, pinned to one
core, load 9-10 from other agents: indicative only, single values move by 10-30% at this load)
| bench | m | pgo |
|---|---|---|
| typing_runtime_protocols | 1.22 | 0.87 |
| deepcopy | 1.55 | 1.31 |
| logging | 1.77 | 1.67 |
| comprehensions | 1.24 | 0.68 |
| nqueens | 1.23 | 0.87 |
| mdp | 1.33 | 1.25 |
| bpe_tokeniser | 1.29 | 1.13 |
| xml_etree | 1.10 | 0.87 |
| pathlib | 1.13 | 1.03 |
| base64 | 1.21 | 0.95 |
| async_generators | 0.72 | 0.50 |
| go | 1.05 | 0.94 |
| generators | 1.08 | 1.64 (outlier: pgo runs 11% fewer instructions, 261.5M vs 292.4M) |
| deltablue | 0.96 | 0.68 |
A single full-list run (one run each, same conditions, both CPython builds): pgo beats the
best CPython build on barnes_hut, chaos, coroutines, decimal_pi, fannkuch, float,
gc_collect, gc_traversal, hexiom, json_dumps, json_loads, meteor_contest, nbody, pidigits,
pprint, raytrace, regex_compile/dna/effbot/v8, richards, richards_super, scimark,
spectral_norm, sqlite_synth, telco, unpack_sequence; behind in that run: asyncio_tcp 1.29,
go 1.21, concurrent_imap 1.14, deltablue 1.13, generators 1.10, python_startup 1.09 (ft
CPython's start-up), plus the list above. argparse, async_tree, crypto_pyaes, pickle need
packages (ERROR for every binary).

### Regression check of today's merges (instructions per loop)
deltablue 36.97M (perf5) -> 40.30M (base, after scale2/perf6) -> 40.65M (head, NORELAX):
+10% since perf5, inside the generic call/attribute paths (not investigated further);
go 1030.9M -> 1064.9M (+3%, CPython 1138M); generators 274.6M -> 292.4M (+6%, CPython
337M). Jump relaxation adds compile time inside short windows (deltablue 1/3 loops: 43.2M
with, 40.65M without; resolve() is ~13.6M over 3 loops after making it near-linear).

### Start-up and memory (warm page cache: the binary read once before; cold runs differ by ~1 MB)
| | base | m | pgo | CPython |
|---|---|---|---|---|
| start-up RSS file / anon | 7.7-8.0 / 3.87 MB | 7.1-7.5 / 3.88 MB | 7.6 / 3.91 MB | 5.96 / 3.28 MB |
| peak (startup_mem.sh workload) | 35.4-35.7 MB | 35.5-35.9 MB | 36.1-36.3 MB | 32.4 MB |
| `-c pass` wall, best of 30 (load 5) | - | 9.8 ms | 9.5 ms | 11.0 ms |
Before regenerating after the core merge, m read 8.5 MB / 36.6 MB: one merge's worth of
renamed/re-inlined functions (174 of 1944 start-up functions missing from the list before
the merge) cost 1 MB, hence the test.

## Correctness
tools/jit_tests/run.sh and tools/stress/run.sh pass on the normal and the PGO build (after
the core merge, and again after the last relaxation change). Spec gate (tools/run_tests.sh
on installed snapshots, the 29 modules of the brief incl. test_xml_etree_c): start of perf7
vs the merged head (before the last two relaxation-speed commits, which only change how
x64.rs picks short jumps) and vs its PGO build: no new failure; differences are improvements that came with
`core` (test_dict, test_sys pass now; test_asyncio runs 22 more tests, same 2 failures;
test_typing +2 tests). test_free_threading crashed once (test_json worker mutating a dict
while it is encoded, load 9.5) on an intermediate build; 12 reruns (6 on the start of
perf7, 6 on that build) and both final gate runs pass - a rare race, probably older than
perf7 (not reproduced). The new x64 unit test and crates/launcher/tests/startup_order.rs
pass.

## What is left (measured causes)
* L1i misses are still 3-5x CPython's on the call-heavy benchmarks even with PGO
  (deepcopy 59K vs 12K per loop, logging 48K vs 12K): (1) JIT code size - tier 2 makes
  ~400 bytes per bytecode for attribute-heavy straight-line code (LogRecord.__init__ 106 KB;
  29% of its bytes were jumps before relaxation, 7% `movabs` of 64-bit constants and helper
  addresses; every helper call is `movabs rax, imm64; call rax`, 12 bytes - placing JIT code
  within +-2 GB of the executable would allow `call rel32` and RIP-relative constants);
  generic call sites inline several alternatives (bound method, function, generic) where
  one is ever taken; (2) runtime code spread: ~180 KB of Rust code per deepcopy loop.
* h_call's own cost (~105 instructions per call: generic dispatch, release loop) plus
  vectorcall_raw (~55) for calls of classes, method-wrappers and builtins from compiled
  code: typing makes ~1000 such calls per loop. A per-site specialisation in tier 1 (type
  guard + direct vectorcall pointer) is the next step.
* Generator expressions: creation + resumption from Rust + zip's two Vec allocations:
  9.2K vs 5.8K instructions for a 4-item `all(... for a, b in zip(...))`.
* Enum compare / `c += bool` go through the generic compare and binary-op helpers
  (comp.py enumcmp 843 vs 649).
* deltablue +10% instructions since perf5 (see above).
* Peak memory: start-up file-backed RSS is the gap (7.1-7.6 vs 6.0 MB; anon 3.9 vs 3.3);
  .rodata is mostly resident through fault-around (startup touches strings and the
  start-up image all over it); text ~4.5 MB resident. The PGO build adds ~0.6 MB at peak
  (hot code is bigger). Ordering .rodata needs stable names (most constants are `anon.*`).
* Machine function splitting with the profile (`-enable-split-machine-functions`) was
  tried: smaller hot text, but start-up code is profile-cold, its blocks went far away and
  start-up RSS grew by 2 MB; not used.
* PGO workflow: profiles go stale as code changes (functions whose hash changed get no
  profile); build.sh regenerates by default (~25 min).
