# pdb1 workstream notes

Branch `worktree-agent-a5b03534ea6bd85ee` (from core 4fb8139). Scope: pdb, bdb, remote pdb, profiling.sampling.

## Results (installed-layout snapshot, `-m test`)
* test_pdb: did not load (`_pyrepl_available` missing; with it: 64 failures + 176 errors and hangs) ->
  245 run, 0 failures, 12 skipped. The 12 skips are the PyREPL tests (`PdbTestReadlinePyREPL`, `test_gen_colors`),
  which need a terminal in the test process (skipped on CPython too under regrtest). Forced on with a driver
  (`pdb._pyrepl_available = lambda: True`): 11/13 pass; test_multiline_auto_indent and
  test_multiline_indent_completion fail because our `_pyrepl` does not treat `\x08` as backspace / a tab as
  "indent" in its keymap (tty2's area; pdb's side is done).
* test_remote_pdb: skipped as a whole (`sys.is_remote_debug_enabled()` was False) -> 49 run, 0 failures,
  4 skipped (PdbAttachTestCase needs `_remote_debugging.PROCESS_VM_READV_SUPPORTED`; one test needs -OO).
* test_profiling: failed to load (no `profiling.sampling`) -> 113 run, 0 failures, 15 skipped: every module of
  test_sampling_profiler raises SkipTest without `_remote_debugging`; the live-collector modules need the
  curses resource. test_heatmap, test_dump, test_sync_coordinator and test_tracing_profiler run and pass.
* test_bdb: passes (unchanged). Regression batch (test_bdb test_sys_settrace test_monitoring test_traceback
  test_inspect test_doctest test_unittest test_cmd test_code_module test_repl test_pyrepl test_asyncio
  test_threading test_sys + test_descr test_pydoc test_reprlib test_types test_builtin test_collections
  test_functools test_capi test_rlcompleter test_audit): same FAIL/ERROR lists as the starting snapshot.
  tools/jit_tests/run.sh: all ok.

## Lib
* Lib/pdb.py: written from scratch. Structure: helpers (find_function, _first_body_line, lasti2lineno),
  targets (_ScriptTarget/_ModuleTarget/_ZipTarget), Pdb (bdb hooks, interaction/setup with chained
  exceptions, a command loop with a hook for the next line, deferred frame status while queued commands
  run, breakpoint command lists queued as `_BpCommand` strings that do not change `lastcmd`, convenience
  variables via an own `$name` scanner, closure-aware execution built from an AST (`_run_in_closure`),
  await at the prompt of `set_trace_async`, completion for readline and _pyrepl, multi-line blocks with
  auto-indent), PdbPyReplInput, remote debugging (_PdbServer, _PdbClient, _connect, attach), module entry
  points and an own command-line parser (options stop at the program; `--`).
* Lib/bdb.py: kept our design, added the 'monitoring' backend (`_MonitoringTracer`: sys.monitoring events
  turned into settrace-style calls, local trace functions per frame activation keyed by id(frame) so no
  frame is kept alive, resumed generators keep their local function, line events of untraced code disabled
  until the next `restart_events()`), `start_trace/stop_trace/restart_events/disable_current_event`,
  `_prepare_trace` (set_trace without installing the event source), set_trace stops in its caller even in a
  skipped module, a step/next/until never stops again on the line it was issued on (gets over one-line
  loops and comprehensions), `enterframe` only set while an event is handled, set_break makes running
  frames with a breakpoint traced, `_set_caller_tracefunc` never traces the bottom frame.
* Lib/profiling/sampling/: `__init__`, `constants`, `collector` (Collector base, extract_lineno,
  normalize_location), `heatmap_collector` (HeatmapCollector, path classification, own HTML/CSS/JS pages),
  `dump` (format_stack_dump), `_sync_coordinator`.
* Lib/_colorize.py (outside my paths, small): new `ProfilerDump` theme section (`profiler_dump`), needed by
  profiling.sampling.dump and pinned by test_dump.

## Runtime (all flagged "pdb1")
* interp/jump.rs: popping NULL stack entries is allowed when jumping (out of a multi-line call).
* interp/eval.rs: after a trace function's jump the target line counts as reported (no second line event);
  the STOP_ITERATION monitoring event of FOR_ITER carries the generator's return value.
* modules/sys_trace.rs: a 'call' trace result of None keeps the frame's local trace function (CPython rule;
  resumed generators stay traced); nothing more is reported for an instruction after a callback jumped;
  STOP_ITERATION/'exception' for a None result is StopIteration() without arguments.
* types/frameobject.rs: f_lineno reports the target of a pending jump.
* types/descr.rs: descriptor repr uses the owner's full tp_name (`<method 'disable' of '_lsprof.Profiler'
  objects>`, pstats labels).
* modules/sys_full.rs: `sys.is_remote_debug_enabled()` is True unless PYTHON_DISABLE_REMOTE_DEBUG /
  `-X disable-remote-debug`.

## Left (causes)
* `_remote_debugging` (RemoteUnwinder over another process's frames, BinaryWriter/BinaryReader, thread
  status, get_child_pids...) and the rest of profiling.sampling (sample.py, cli, pstats/collapsed/flamegraph/
  gecko/jsonl/diff/binary/live collectors, _child_monitor, errors). Adding `_remote_debugging` turns on all of
  test_sampling_profiler (~14k lines of tests), so it was not started; this is a workstream of its own (needs
  an exported table of our frame/code layout for process_vm_readv, or a cooperative sampling channel).
* `sys.remote_exec` still raises RuntimeError, so `pdb -p PID` reports "Cannot attach"; its tests are gated on
  `_remote_debugging` as well.
* PyREPL keymap (see above).

## Known quirks / workarounds
* `Pdb._resumed_coroutine`: with generator chains the runtime reports the resumption of a coroutine that was
  suspended by an awaitable run at the `set_trace_async` prompt only when the inner coroutine returns; pdb
  ignores that one late 'call' event.
* `Pdb._is_jump_target`: set_trace stops at the next instruction that is not a jump target (CPython's
  "an instruction with a line number" for `x and pdb.set_trace()`; our compiler gives the join point a line).
* Lib/codeop.py drops `flags` in its final compile; pdb compiles top-level-await input itself.
* test_cprofile in test_profiling needed the descriptor repr fix above (was failing before).
