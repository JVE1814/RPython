# mem1 workstream: start-up and peak memory

Branch: the mem1 worktree (master + `core` = perf7, merged at the start and again at the
end). Everything written from scratch. Method: which pages of the executable are resident
(`/proc/PID/pagemap`, read from outside the process), which pages a run really touches
(valgrind lackey), which functions a native run executes and in which order (gdb), allocator
statistics. The baselines only through tools/bench scripts.

## What the harness measures

`tools/bench/startup_mem.sh` (the benchmark harness's memory numbers):
* start-up: a process started as `PY -c 'for line in open("/proc/self/status"): ...'`
  prints its VmRSS, RssAnon and RssFile. By then it has started up, compiled the `-c` code,
  opened a text file and run a short loop (str.startswith/split/lower, print).
* peak: `VmHWM`, the high-water mark of RSS (anonymous + file-backed), of a process that runs
  `x = [list(range(100)) for _ in range(10000)]; d = {i: str(i) for i in range(100000)}`.
  So peak = start-up RSS + the workload's objects + the code and data pages the workload
  adds + any transient (a dict resize holding the old and the new table at once).

## Findings

* **File-backed RSS grows in 64-128 KB steps.** On a page fault the kernel maps the whole
  fault-around window (64 KB, aligned in the address space) and, for large page-cache folios,
  up to 128 KB. A single function or constant outside the start-up area therefore costs 64-128
  KB. CPython's executable has the same effect; it is just smaller (3 MB of text against
  our 10 MB).
* **perf7's start-up order had decayed after the merge.** Eight start-up functions had been
  renamed or re-inlined, which cost one window each, about 1 MB in total. The order also
  missed functions that only a native run executes. Valgrind runs take slightly different
  paths, and callgrind names functions under other aliases. An example is the SIGINT handler
  installation: a run started in the background inherits SIGINT ignored and skips it, which
  hid `default_int_handler_obj`. `_start` and the crt helpers sat at the start of `.text`, in
  a window of their own.
* **.rodata was 2.5 MB and almost all resident** (2.1 MB). The start-up image (622 KB), the
  frozen modules' sources (300 KB), the Unicode name tables (520 KB), the decomposition
  tables, the CJK codec tables (~250 KB), the CRC tables, and the `_pydatetime`/`getpath`
  sources were interleaved with the constants start-up reads. Exact touches (lackey):
  start-up reads about 480 KB of .rodata outside the image.
* **The peak was the last dict resize.** The old table (2.6 MB, fully written) and the new
  one (indices plus the copied entries, 3.1 MB) were resident at the same time:
  HWM − final RSS = 0.8 MB.
* **The workload's objects are fixed by the free-threaded ABI.** In CPython's
  free-threaded layout the object header is 32 bytes (16 in the default build), so an int is
  48 bytes (32 in the default build) and `str(i)` is 64 bytes (48 in the default build). That
  is +3.2 MB against py-default for the 100k ints and strs. Our dict touches only the entries
  it uses (in CPython the whole 5.1 MB keys block is resident at the end), which saves
  1.8 MB. The lists come out even
  (800-byte item arrays, which took an 832-byte class before).
* **Anonymous memory at start-up** (3.9 MB against 3.3 MB): allocator pages 1.9 MB (CPython's
  arenas: 1.5 MB, with smaller headers), malloc heap 1.05 MB (0.8 MB), .bss 0.29 MB (the method
  cache, 96 KB, plus static type objects), and .data.rel.ro 0.32 MB. .data.rel.ro is all
  relocated, so all of it is dirty (CPython's is 0.17 MB, plus 0.55 MB of .data).
* Start-up imports 27 modules (sys.modules after `-c pass`); nothing to cut there.
* About 130 KB of `.gcc_except_table`/`.eh_frame_hdr` becomes resident during start-up
  without any user-space read (a PROT_NONE mprotect of the range raised no fault, and no
  syscall touches it). The cause is not found; it is not ours to fix by layout.

## Changes

### Read-only data layout (crates/launcher/docs.ld)
After `.rodata`, in this order:
* `.rodata.rpyimg`: the start-up image (`startup_image/mod.rs`). Its layout is now
  [the start-up modules' records][area 0: their module and class bodies][area 1: their
  functions, then every translated form of the other modules][the other modules' records].
  Module offsets in the index are absolute, so the runtime reads both forms unchanged. What a
  start does not read is one piece at the end.
* `.rodata.rpydoc`: method docs (perf7). str.maketrans's doc moved out of it: type_ready
  wraps static methods in a staticmethod object, which copies `__doc__`. That one read pulled
  in a 64-128 KB window of docs at every start.
* `.rodata.rpycold`: blobs a normal start never reads. New macros `cold_bytes!`, `cold_str!`
  and `cold_slice!` (crates/runtime/src/methods.rs) put them there: the frozen modules'
  sources (frozen.rs), `_pydatetime`'s and getpath.py's sources, and the CJK codec tables'
  code-point arrays (tools/gencjk emits `cold_slice!`). Also there: the Unicode name tables,
  the decomposition tables and `COMP_PAIRS` (crates/unicode/gen marks them; the crate's
  `forbid(unsafe_code)` became `deny` with an allow on the generated tables, because
  `#[link_section]` counts as unsafe), and zlib's and lzma's CRC tables.
Result: `.rodata` shrinks from 2.5 MB to 0.75 MB. All of it is still resident (start-up reads
~480 KB of it, spread out); the image is 540 of 622 KB; the docs and the cold section are not
touched.

### Start-up code order (tools/bench/perf/gen_order.sh, crates/launcher/startup.ld)
* The start-up runs are recorded **natively** by the new `tools/bench/perf/native_funcs.sh`:
  gdb sets a temporary breakpoint on every function symbol, and each fires once, which gives
  the first-execution order. It takes 1.5-2.5 minutes a run. The micro benchmarks still go
  through callgrind. Without gdb (or with `ORDER_CALLGRIND=1`) everything goes through
  callgrind as before.
* The runs, in order: `-c pass`; `-S -c pass`; a file read with the common str operations;
  a small script (comprehensions of each kind, int/str conversion, sorting, enough allocation
  for the cycle collector and the eval breaker); then the common imports.
* `_start` and the crt init/fini helpers (`*crt1.o`, `*crti.o`, `*crtbegin*.o`) go first.
  At the start of `.text` they took a window of their own.
* The runs get the default SIGINT disposition (`env --default-signal=INT`). A script started
  in the background has SIGINT ignored, and python then skips installing its handler.
* `ORDER_REFRESH=1` re-records only the start-up runs and keeps the rest of the current list
  after them: about 10 minutes instead of 30+. Use it after every merge. Each renamed or
  re-inlined start-up function costs a 64-128 KB window until the list is refreshed.
* The list is compared and written with the crate hashes masked (`Cs*_`), as before.

### Peak
* Dict resize (`types/dict.rs`): a data block of at least 512 KB, when no other thread has
  read the table (`shared_read` false), gives its old pages back while it is copied:
  first the old index array (the copy reads only the entries), then each 8192-entry chunk of
  copied entries (`madvise(MADV_DONTNEED)` on whole pages inside the malloc block; the pages
  holding the block's first and last bytes are kept). The old and the new table are no
  longer both fully resident. HWM now equals the final RSS, where it was 0.8 MB above it.
  The copy loop runs per chunk, so its instruction count is unchanged.
* Allocator size classes (`tlab.rs`): 32-byte steps from 384 to 1024 bytes, 128-byte steps
  to 2048 (were 64 and 256; 50 classes instead of 36). The rounding loss is now at most ~11%
  instead of 25%. A 100-item list's item array is 800 bytes instead of 832, saving 320 KB in
  the workload. Unused classes cost nothing (pages are carved lazily).

### Tools
* `tools/bench/smaps_split.sh PY [start|-S|peak|code]`: RSS and anonymous memory per mapping,
  read from outside the process (also for the baselines).
* `tools/bench/perf/resident_syms.sh PY [code] [section-regex] [min-kB]`: resident pages of
  the executable per section and per symbol, from /proc/PID/pagemap read from outside the
  process. A Python loop over the pages inside the process (as in perf7's text_pages.py)
  starts the JIT and faults in about 1 MB of its code, which then shows up as "resident at
  start-up". `RESIDENT_PAGES=`/`PAGES_FROM=` save or reuse page lists.
* `tools/bench/perf/touched_pages.sh PY OUT args`: the pages a run really reads or writes
  (valgrind lackey; `INSTR=1` adds instruction fetches). Unlike residency, it is not blurred
  by fault-around.
* `tools/bench/perf/native_funcs.sh PY OUT args`: the functions a native run executes, in
  first-execution order.
* `tools/bench/perf/resident_bss.sh PY [code]`: resident pages of .data/.bss/.data.rel.ro per
  symbol.
* `tools/bench/mem_compare.sh N PY...`: startup_mem.sh's numbers, interleaved, min-max per
  build.
* `sys._debugmallocstats()` also prints the allocator's pages, carved bytes and cached free
  blocks per (kind, size class).

## Decision proposal D37: memory layout of the executable
* Data that a normal start does not read goes in `.rodata.rpycold` (`cold_bytes!`/
  `cold_str!`/`cold_slice!`, or `#[link_section = ".rodata.rpycold"]` in generators). The
  start-up image goes in `.rodata.rpyimg`, method docs in `.rodata.rpydoc` (perf7); all are
  placed after `.rodata` by crates/launcher/docs.ld. New big constant tables or embedded
  sources should use these.
* The start-up function order comes from native runs in first-execution order. After every
  merge into core, refresh it with `ORDER_REFRESH=1 tools/bench/perf/gen_order.sh
  <snapshot>/bin/python3` (~10 min). The `startup_order` test only notices decay of more than
  10%, but one missing function already costs 64-128 KB.
* Memory is compared with `tools/bench/mem_compare.sh`, interleaved with the baselines, on a
  warm page cache. Residency questions go through resident_syms.sh, touched_pages.sh and
  native_funcs.sh rather than in-process scripts.

## Tried and dropped
* LLVM's HotColdSplitting (`-C llvm-args=-hot-cold-split=true`, without a profile): it split
  off 3284 cold blocks, but the start-up area shrank by only 12 KB (static heuristics find
  little cold code on the start-up paths). Not worth a global codegen change.
* MADV_RANDOM on the segments: per perf6 it changes nothing on this kernel. Fault-around does
  not look at it.
* Ordering the start-up image's function translations by first use: start-up touches 455 of
  622 KB of the image, and the untouched part is now one piece at the end. The remaining holes
  are worth at most one 64 KB window.
* Non-PIE linking would keep .data.rel.ro clean (0.32 MB anonymous now, all relocated), but
  CPython's builds are PIE as well, and ASLR matters more.

## Measurements
startup_mem.sh through `tools/bench/mem_compare.sh 4` (interleaved rounds, min-max, warm page
cache; the machine was loaded at 12-17, which matters for wall times but not for RSS). "base"
is the start of mem1 (core merged, snapshot of that build); "mem1" is the final branch head
(core merged again, start-up order regenerated); all are installed snapshots
(tools/snapshot.sh).

| build | start-up VmRSS | RssAnon | RssFile | peak VmHWM |
|---|---|---|---|---|
| CPython py-default | 9.07-9.43 MB | 3.28 MB | 5.79-6.15 MB | 32.26-32.37 MB |
| CPython py-ft | 10.68-10.76 MB | 4.83-4.85 MB | 5.84-5.92 MB | 38.71-38.78 MB |
| base (normal build) | 11.57-11.65 MB | 3.88 MB | 7.68-7.77 MB | 36.22-36.36 MB |
| **mem1 (normal build)** | **9.64-9.71 MB** | 3.96 MB | **5.68-5.75 MB** | **32.99-33.38 MB** |
| mem1 (PGO build) | PGO_START | PGO_ANON | PGO_FILE | PGO_PEAK |

The executable's own resident pages right after start-up (resident_syms.sh): 6.3 MB at
base, 3.98 MB at the end (CPython: 4.19 MB):

| | base | mem1 | CPython |
|---|---|---|---|
| .rodata (+ image, docs, cold sections) | 2.06 + 0.11 MB | 0.76 + 0.54 + 0 + 0 MB | 1.08 MB (+ 0.49 MB .rela.dyn) |
| text | 2.53 + 1.03 MB (start-up area + strays) | 2.06 + 0.02 MB | 1.71 MB |
| .data.rel.ro / .data | 0.32 / 0.03 MB | 0.32 / 0.03 MB | 0.18 / 0.29 MB (+0.29 .PyRuntime) |

Instruction counts (callgrind) are unchanged by mem1's changes: `-c pass` 40.11M → 40.15M,
`-S -c pass` 19.74M → 19.72M; the peak workload 411.99M (base) → 408.93M (head);
micro benchmarks, comparing pre-merge builds: listdict 148.58M → 148.59M, attr 78.08M →
78.08M. In the merged head listdict is 155.6M: that +5% comes with core's merge (scale3/tail4
dict and GC changes), not with mem1's changes; the other micro benchmarks are within +0.6%.

## What is still behind, and why
* **Anonymous memory at start-up: +0.68 MB compared with py-default.** The objects carry
  the free-threaded 32-byte header (allocator: 1.13 MB of raw blocks and 0.61 MB of GC
  objects at start-up). The malloc heap holds the Rust-side tables (inline caches, decoded
  translations, method tables built at run time, the intern table: ~0.75 MB live, 1.05 MB
  resident). .data.rel.ro is fully relocated (0.32 MB dirty). Ideas not done: pointer-free
  Unicode alias/named-sequence tables (34 KB of relocated data), a smaller carve batch in the
  allocator (≤ 50 KB), lazy type `__doc__` strings (CPython creates them eagerly too).
* **Peak: +0.7-1.0 MB compared with py-default** (and 5.4 MB below py-ft). The workload's
  objects alone are 1.4 MB bigger than CPython's default build: +3.2 MB from the
  free-threaded int and str sizes, −1.8 MB because our dict does not zero its entry array.
  On top of that come the start-up difference and ~0.5 MB of code that the workload adds
  (comprehension compiling, the cycle collector, the eval breaker). CPython's own peak
  includes a 1.5 MB resize transient that ours no longer has. Beating py-default here would
  need the start-up anonymous memory 1 MB below CPython's, which the ABI's object sizes do not
  allow.
* The start-up order decays with every merge that renames start-up functions: the core
  merge in the middle of this work cost 1.3 MB of start-up RSS until the list was refreshed.
* The ~130 KB `.gcc_except_table`/`.eh_frame_hdr` window of unknown origin (see Findings).

TODO
