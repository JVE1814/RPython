# remote1 workstream notes

Branch `worktree-agent-a93a94a9147163090` (from core f24eaa3). Scope: `_remote_debugging`,
`sys.remote_exec` (PEP 768), `profiling.sampling`, test_pyrepl diffs.

## Incident (AGENTS.md rule 2)
2026-11-07: I ran `python3 --version` TWICE, each at the start of an unrelated chained shell
command, by reflex (output `Python 3.13.18`, unused; nothing else executed with the host
interpreter). Flagging for the DECISIONS.md D12 incident log. No other Python interpreter
was run; no artefact was produced or tested with it.

## Results (installed-layout snapshot, `-m test`), before -> after
* test_external_inspection: SKIP (0 run) -> PASS 67 run / 0 fail / 2 skip (matches baseline).
* test_remote_pdb: PASS 49 run / 4 skip -> PASS 49 run / 1 skip (PdbAttachTestCase now runs).
* test_sys: remote-exec tests were skipped (24 skip) -> 9 skip (TestRemoteExec runs & passes);
  the 1 pre-existing failure (sizeof) + 1 pre-existing error (subinterp intern) are unchanged.
* test_audit: the sys.remote_exec audit test now runs (5 -> 4 skip).
* test_profiling: was PASS 113 run / 15 skip; adding _remote_debugging made its test files
  import profiling.sampling.* and CRASH the loader (0 run). Rebuilt the package -> 483 run /
  ~98 fail / ~129 error / 7 skip. pstats/collapsed/jsonl collectors, heatmap and tracing pass;
  the remaining fail/error need the rich flamegraph/gecko/binary formats (incl. a Rust
  `_remote_debugging.BinaryWriter`/`BinaryReader`), the full async `_iter_async_frames`
  reconstruction, and the CLI coordinator paths.
* Regression batch (no new failures vs starting snapshot): test_pdb 245/0, test_bdb 37/0,
  test_sys_settrace 448/0, test_monitoring 100/0, test_threading 247/0, test_asyncio
  2929 run/10 fail (was 2907/10 — +22 run, same failures), test_capi 1534/40 (unchanged),
  test_inspect 382/0, test_traceback 441/0, test_frame 61/0, test_cmd_line 82/0. jit_tests all
  ok; stress all PASS.

## Design: the debug table (D-analogue of `_Py_DebugOffsets`)
`crates/runtime/src/modules/remote_debug/offsets.rs` defines `DebugTable`, a `#[repr(C)]`
static `RPY_DEBUG_TABLE` placed in its own ELF section `.rpy_debug` (`#[link_section]`,
`#[used]`, `#[no_mangle]`). An out-of-process reader:
1. reads `/proc/PID/exe`, parses the ELF section headers to find `.rpy_debug`'s link-time
   address + size (procmem.rs `find_section`), adds the PIE load bias from `/proc/PID/maps`;
2. copies the table with `process_vm_readv`, checks magic (`RPyDbgT\0`), version and size;
3. reads every other address/offset from the table.

Table contents (all computed with `offset_of!`/`size_of!`, re-checked by
`offsets::tests::table_matches_layouts`):
* globals' addresses: the qsbr thread-state mirror (`LIST`/`LIST_LEN`/`LIST_VER`/`CAP`,
  a seq-lock), `MAIN_THREAD_STATE`, the remote-exec mailbox `REMOTE_EXEC`, the asyncio task
  list head `target::TASK_HEAD`, the set `DUMMY_CELL`, `None`, and the type objects
  (code/gen/coro/asyncgen/asend/athrow/str/long);
* struct field offsets for `ThreadState`/`CThreadState`, `ErrStackItem`, `Frame`,
  `PyCodeObject`+`CodeData`+`CodeMeta`, and object layouts (PEP393 str, int, bytes, tuple,
  set, gen frame, asend/athrow gen, type name/flags, interp id);
* run-time-filled fields: the `Vec<T>` pointer/len word offsets (std does not promise an
  order, so `init_table` measures them), and the `flags` word (bit0 = remote debugging
  enabled). `init_table()` runs late in `lib.rs::initialize` and sets `ready=1`.
* the asyncio slot offsets (`AsyncioOffsets`, filled by `_rd_layout` when `_asyncio`
  imports; member-descriptor `.offset` of `_task_name`/`_task_coro`/`_fut_awaited_by`/
  `_fut_state`).

VERSION is bumped on any `DebugTable` layout change.

## `_remote_debugging` (Rust builtin, `reader.rs` + `reader/asyncwalk.rs`)
Public API (derived from spec tests):
* `PROCESS_VM_READV_SUPPORTED` (True), `THREAD_STATUS_*` constants.
* struct-sequence types `FrameInfo`, `LocationInfo`, `ThreadInfo`, `InterpreterInfo`,
  `CoroInfo`, `TaskInfo`, `AwaitedInfo` (via `types::structseq`; constructible `T(seq)`).
* `is_python_process(pid)`, `get_child_pids(pid)` (recursive, from /proc), `zstd_available()`.
* `RemoteUnwinder(pid, *, all_threads=False, only_active_thread=False, debug=False,
  cache_frames=False, mode=0, native=False, skip_non_matching_threads=True, opcodes=False,
  stats=False)` with `.get_stack_trace()`, `.get_all_awaited_by()`,
  `.get_async_stack_trace()`, `.get_stats()`.

Sync unwinding (`get_stack_trace`): enumerate thread states (all via the seq-locked mirror,
or just the main thread), group by interpreter id, walk each thread's Rust `Frame` chain
(`ts.frame` Cell) innermost-first, map internal `lasti` -> public code-unit offset via
`CodeData.pub_of_internal`, decode `co_linetable` for the LocationInfo, read qualname/
filename (truncated to 1024), public opcode from `CodeMeta.co_code`. Thread status: HAS_GIL
= mode InVm; ON_CPU = /proc task stat 'R'; HAS_EXCEPTION = current_exception set or any
exc_info chain value non-None; MAIN_THREAD = ts == main. Frame cache keyed by
(native_id, frame_addr) validated against (code, lasti) so parents keep object identity and
pool-reused addresses rebuild.

Async (`reader/asyncwalk.rs`, best effort): walk the target-side task registry
(`target::TASK_HEAD` linked list of `TaskNode`), per task build coroutine_stack (running
task -> live thread frame segment up to the coro frame; suspended -> follow the frame's
yield-from delegate down the gen/coro/asend chain) and awaited_by from the `_fut_awaited_by`
set (skipping the dummy tombstone). Grouped by the creating thread's native id + a (0, [])
per-interp fallback. Limits raise "Too many task waiters / async stack frames / coroutine
frames".

## PEP 768 (`sys.remote_exec` + eval breaker)
* `offsets::EVAL_REMOTE_EXEC = 1<<56` (top byte of the eval-breaker word, so a debugger's
  single-byte store does not race the runtime's atomic RMWs). `target::run_pending` runs the
  queued script at the next safe point (opens via `io.open_code`, compiles, execs in a fresh
  namespace, audit `cpython.remote_debugger_script`, errors reported unraisable).
* `sys.remote_exec(pid, script)`: validates (TypeError non-path, OSError bad pid/missing
  file), audits `sys.remote_exec`; self -> queue + set own main-thread bit; other process ->
  `reader::do_remote_exec` writes the mailbox + sets the target main thread's bit (RuntimeError
  "Remote debugging is not enabled in the remote process" if the target disabled it).
* `-X disable-remote-debug` / `PYTHON_DISABLE_REMOTE_DEBUG` already gate
  `remote_debug_enabled()` (pdb1); `init_table` copies that into the table's flags.

## Runtime touch points (flag "remote1")
* `_asyncio.rs`: eager task naming (CPython C behaviour, so a debugger always sees a name);
  `_rd_link/_rd_unlink/_rd_running/_rd_layout` hooks in the task registry functions;
  `install_asyncio_hooks` injected at module init.
* `interp/eval.rs::handle_eval_breaker`: handle `EVAL_REMOTE_EXEC`.
* `lib.rs::initialize`: `remote_debug::init_table()`.
* `qsbr.rs`: `LIST`/`LIST_LEN`/`LIST_VER`/`CAP` made `pub(crate)` (addresses published).
* `types/set.rs`: `DUMMY_CELL` `pub(crate)`. `types/asyncgen.rs`: `AWAITABLE_GEN_OFFSET`.

## Status / what's left
* profiling.sampling collectors (sample.py, cli, pstats/collapsed/flamegraph/gecko/jsonl/
  diff/binary/live, errors, opcode_utils, _child_monitor) NOT written yet — the biggest
  remaining piece for test_profiling.
* async walker is best-effort; column offsets of LocationInfo depend on our compiler.
* test_pyrepl keymap fix not started.
