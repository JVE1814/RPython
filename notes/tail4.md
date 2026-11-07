# tail4 — profiling.sampling, _remote_debugging binary format, long-tail fixes

Measured on installed-layout snapshots (tools/snapshot.sh). Start: core d620c3b (remote1 merged);
core merged again (capi3, refs1) before hand-back.

## test_profiling (483 run: 97 fail / 129 err / 7 skip -> 1 fail / 0 err / 6 skip)

### `_remote_debugging.BinaryWriter` / `BinaryReader` (crates/runtime/src/modules/remote_debug/binfmt.rs)
Own format, derived from test_binary_format (offsets the tests patch are fixed):
* 64-byte header: magic u32 0x54414348 (bytes "HCAT" on little endian), version 1, Python hex
  version, start timestamp, interval, thread-sample count (u64 @28), distinct (thread, interp)
  pairs (u32 @36), string/frame table offsets (@40/@48), compression (0 none, 1 zstd), mode byte
  (0xff unknown), capture-config flags byte (bit 7 = recorded).
* records from offset 64: u64 thread id, u32 interpreter id, u8 kind. Kind 0 = repeat of the
  thread's previous stack: varint n + n x (varint timestamp delta, u8 status); kinds 1/2/3 = full
  stack / frames pushed on the previous stack / frames popped then pushed. Deltas are per thread.
  The writer keeps a pending repeat run per thread (flushed at 8192 samples, on a different stack
  and at finalize); zstd compresses the record stream in 1 MiB frames (decompress handles
  concatenated frames).
* string table (varint length + UTF-8, lone surrogates preserved), frame table (varint ids, line
  code 0 = no location else zigzag+1, zigzag end-line delta / columns, opcode byte 0xff = None).
* optional measured-stats block ending in "TACHSTAT" u32 version u32 size (v1: duration, rate;
  v2 adds error rate, missed samples and a presence mask); 32-byte footer (string count, frame
  count, file size, tag).
* Writer: `write_sample` validates each interpreter fully before committing it (an interpreter id
  > u32 raises OverflowError and sets `limit_reached`, the writer stays usable); any other error
  breaks the writer (later calls: ValueError "broken"); `finalize`, `close` (discard),
  `__enter__/__exit__` (finalizes unless broken), `set_stats`, `get_stats`, `total_samples`.
* Reader: validates on open (magic, version, file size, table offsets, string/frame counts against
  the file size), tables parsed lazily at `replay(collector, progress_callback=None)`; replay calls
  `collector.collect([InterpreterInfo((iid, [ThreadInfo])) ], timestamps)` per record, repeat runs
  split at status changes and 8192 samples; thread-count and sample-count mismatches and trailing
  bytes are ValueErrors with the tests' messages.

### profiling.sampling (Lib/profiling/sampling)
Rewritten: `sample.py` (sampling loop: slots at start + n*interval, aggregating collectors get runs
of identical snapshots as `collect(frames, timestamps_us=[...])` up to MAX_PENDING_SAMPLES, others
`collect(frames)`; stats print; KeyboardInterrupt; `dump_stack`; blocking mode), `cli.py`
(run/attach/dump/replay subcommands, -r rates like 10khz, --mode choices, pstats-only options,
--async-aware rules, --subprocesses child monitor, coordinator launch through
`_sync_coordinator`), `pstats_collector.print_stats` (units, sort modes, summary sections),
`stack_collector.py` (collapsed; flamegraph tree with per-thread values/opcodes, significance
pruning per thread, root promotion, thread status/GC stats, HTML page; differential flame graph:
function-path matching, baseline scaled by interval ratio, per-line apportioning by inclusive share,
elided baseline-only paths with matched ancestors carrying only inclusive time), `gecko_collector.py`
(processed-profile JSON, GIL/CPU/opcode interval markers, spill files, atomic export),
`jsonl_collector.py` (chunks of 256, location fields elided when unknown), `binary_collector.py` /
`binary_reader.py` (wrappers), `collector._iter_async_frames` (leaf tasks, `<task>` markers,
lowest-id parent, "(N parents)"), `_child_monitor` (limits, waiting message), `__main__.py`.

### `_remote_debugging` reader (reader.rs, flagged tail4)
* Torn frame-chain reads are detected and retried (a frame without its code object, a generator
  frame that lost its caller link, an outermost frame that changed): deep oscillating generators
  now always reach `<module>`. Code cache entries are re-validated (filename/qualname/linetable/
  code-data pointers), so a code object freed and reallocated at the same address is not
  misattributed.
* `gc=` option and `<GC>` frames: debug table v4 publishes `gc::GC_COLLECTOR_TS` (thread state
  running a collection, set for the whole collection including finalizers).
* `pause_threads`/`resume_threads` (SIGSTOP + wait for every task to stop / SIGCONT).
* `get_async_stack_trace` reports the running task of each thread and the tasks waiting for it.
* `GCMonitor(pid).get_gc_stats(all_interpreters=)`, `get_gc_stats(pid, ...)`, `GCStatsInfo`
  (test_gc_stats 8 run / 3 skip = baseline): gc_mod.rs keeps per-(interpreter, generation)
  counters in a fixed table published by the debug table (records of 10 u64 words).

## Other modules
* test_pyrepl 2 -> 0: insert-character uses the counted `ich` capability when `ich1` is absent
  (xterm terminfo) — test_cursor_back_write.
* test_gc 2 -> 0: capsules are GC types (`traverse_func`/`clear_func` in the two words capi3
  reserved, `_PyCapsule_SetTraverse` exported; `_socket.CAPI` tracked); a full collection counts
  live untracked tuples into the long-lived total (we never track tuples of atomics).
* test_descr 2 -> 0: attribute-cache filling never runs `__eq__` (dict `lookup_index_no_eq`,
  undecided -> no cache entry; adaptive view likewise); METH_METHOD descriptors' `__get__` checks
  arg 2 is a type; `_io._TextIOBase.read` takes its defining class.
* test_opcache 1 -> 0: per-type inline key space of 21 distinct names (2/3 of a 32-slot table);
  an attribute name beyond it moves the instance to its own dict (GC_BIT_DICT_DETACHED).
* test_repl 1 -> 0: the asyncio REPL with non-terminal input follows the new REPL's completeness
  rule (a one-line compound statement runs at once; blocks end with a blank line).
* test_sys_settrace 1 err -> 0: f_lineno may be set at a yield's 'return' event; the eval loop
  then drops the value and continues at the target instead of suspending.
* test_thread_local_bytecode 1 -> 0: the JIT's generic BINARY_OP helper records into the running
  thread's specialisation view (helpers.rs, flagged tail4).
* test_asyncio 2 -> 0 (sslproto *_failed_certificate_no_cycles): (1) the start_tls waiter was kept
  by a QSBR-deferred decref of a replaced dict value; QSBR now releases at once when every other
  thread is quiescent (at retire time for dict value replacement: `qsbr::release_unpublished`, and
  at every poll for everything pending); (2) `_SelectorSocketTransport` drops `_read_ready_cb`
  (a bound method referring back to it) when the connection is lost.
* test_importlib 6 err -> 0 (20 failures = baseline): unittest.mock imports pkgutil at import time
  (patch() resolved targets while test code had replaced sys.path).
* test_build_details 1 fail + 10 err -> 0: tools/build_details.py (run by the installed
  interpreter from tools/snapshot.sh and tools/install.sh) writes the PEP 739
  build-details.json and lib/pkgconfig/python-3.15.pc.

## Left
* test_profiling test_native_frames_enabled: `<native>` frames need to know which Python frames
  were entered from native code (no marker in our Frame; CPython has C-stack shim frames).
* CLASS_KEYS_MAX = 21 is a guess consistent with test_125868 (must be <= 23); classes with more
  distinct attribute names lose inline values for the extra names.
* The asyncio piped-input rule lives in asyncio/__main__.py; the main REPL (pymain) keeps the
  classic blank-line rule for piped input, as CPython's basic REPL.

## Tooling notes
* Scripts in /home/user/RPython/target/tail4/s (t.sh, many.sh, regr.sh, sync.sh, b.sh).
* Bash heredocs combined with cd/variables are refused by the sandbox; keep commands simple.
