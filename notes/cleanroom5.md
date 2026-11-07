# cleanroom5 — continuation of the D19 audit (see notes/cleanroom1..4.md)

## Method
- As before: a module judged recalled is rewritten without reading the old body — only its
  list of top-level names / method signatures (API), the documented behaviour, the spec tests,
  and a grep of crates/ + Lib/ + spec/Lib/test for every private name other code or the tests
  use.  The old file is kept aside (target/cleanroom5/scratch) for black-box comparison.
- "before -> after": full `test_asyncio` (-j3) and the other test modules that import the
  module, on an installed snapshot (target/cleanroom5/snap), old file vs new file, same binary.
- Speed: pyperformance async_tree (none / eager / memoization) and asyncio_tcp through a copy
  of the pyperf shim that forwards argv (the shared shim ignores benchmark arguments, so
  async_tree could not run through tools/bench/run_bench.sh); asyncio_websockets needs the
  third-party websockets package, which is not installed here.  Old/new interleaved, noisy box.
- Scratch scripts in an untracked `.scratch5/` in the worktree.

## Rewritten

### asyncio/base_events.py
- Tests (before -> after): test_asyncio 2929 run, 2 failures -> 2929 run, 2 failures (the same
  two pre-existing test_sslproto `*_failed_certificate_no_cycles`, see sslproto below);
  test_coroutines, test_contextlib_async, test_asyncgen, test_unittest, test_external_inspection,
  test_logging, test_pdb, test_inspect, test_code, test_support, test_syntax, test_warnings,
  test_lazy_import, test_cmd_line, test_os, test_concurrent_futures = (pass);
  test_gc, test_sys_settrace, test_concurrent_futures.test_init fail the same with the old file.
- Kept because pinned: `_ipaddr_info`, `_interleave_addrinfos`, `_set_nodelay`,
  `_check_ssl_socket`, `_HAS_IPv6`, `_MIN_SCHEDULED_TIMER_HANDLES`,
  `_MIN_CANCELLED_TIMER_HANDLES_FRACTION`, `MAXIMUM_SELECT_TIMEOUT`, module globals `socket` and
  `logger` (tests patch them), every `_make_*_transport` hook, `_process_events`,
  `_write_to_self`, `_run_once`, `_run_forever_setup/_cleanup`, `_add_callback(_signalsafe)`,
  `_timer_handle_cancelled`, `_ensure_resolved`, `_create_connection_transport`,
  `_sock_sendfile_native`, `_sendfile_native`, `_check_closed`, `_check_sendfile_params`,
  `_do_shutdown`; loop attributes `_ready`, `_scheduled`, `_timer_cancelled_count`,
  `_clock_resolution`, `_default_executor`, `_debug`, `_internal_fds`, `_closed`,
  `_thread_id`, `slow_callback_duration`; Server's `_sockets`, `_waiters`, `_clients`,
  `_attach/_detach`, `_start_serving`, `_context`; the `_buffered` start_tls parameter
  (hang1); all asserted messages and log formats ("Executing %s took %.3f seconds", the
  exception-handler texts).
- New structure: create_connection's resolution and connect loop moved to `_dial`, local
  binding to `_bind_local`, the final error to `_connect_failure` (returned, then raised by
  the caller); datagram endpoints are planned by `_datagram_address_pairs` and opened by
  `_open_datagram_socket`; listening sockets by `_bind_listening_socket`; both sendfile
  fallbacks share `_copy_file_chunks(send=...)`; pipes share `_connect_pipe`; subprocesses
  share `_check_subprocess_options` + `_spawn`; the timer-heap cleanup is
  `_purge_cancelled_timers`, debug-mode callback timing `_run_ready_debug`; the stand-in
  sendfile protocol tracks write room with one `_room` future.
- Behaviour changes (all tests equal): create_connection() empties its per-attempt error
  lists before raising, so a failed happy-eyeballs attempt's frames no longer keep the
  exception referenced from a list; dropped/renamed private helpers nothing else uses
  (`_run_until_complete_cb` -> `_stop_when_done`, `_format_handle` -> `_describe_handle`,
  `_format_pipe` -> `_pipe_label`, `_thread_pool_class` -> `_thread_pool_type`).
- Speed (s, old vs new, 3 interleaved rounds): async_tree none 1.01/1.07/0.97 vs
  1.15/1.00/0.95, eager 0.35/0.45/0.33 vs 0.36/0.33/0.33, memoization 1.33/1.42/1.26 vs
  1.37/1.13/1.06, asyncio_tcp 0.81/0.97/0.69 vs 1.04/0.67/0.68 (equal within noise).

### asyncio/staggered.py
- Tests: test_staggered 8, test_eager_task_factory, test_base_events, test_streams (incl.
  test_open_connection_happy_eyeball_refcycles), test_external_inspection 67
  (test_async_staggered_race_remote_stack_trace) = (pass); full test_asyncio unchanged.
- Kept because pinned: the local coroutine name `run_one_coro` (its qualname
  `staggered_race.<locals>.run_one_coro` appears in test_external_inspection's expected stacks)
  and the awaited-by registration of each attempt task with the calling task; the
  `(result, index, exceptions)` return shape.
- New structure: one event-driven loop in the race coroutine (flags `launch_now`/`cancelled`,
  a `wake` future poked by attempt completions and the delay timer, `launch()` starting the
  next attempt and re-arming the timer).
- Behaviour: as before; base_events now empties the race's exception list after use (the
  attempt frames, reachable from a raised exception's traceback, reach it through the closure).

### asyncio/sslproto.py
- Verdict: method-for-method CPython's (uvloop-derived) SSLProtocol with reworded comments.
- Tests (before -> after): test_asyncio 2929 run / 2 failures -> same 2 failures; test_ssl
  (asyncio) 24, test_sslproto, test_server_context (incl. test_gh140947), test_streams,
  test_events, test_buffered_proto = (pass).
- The two `*_failed_certificate_no_cycles` failures are not sslproto's: the failed waiter's
  exception keeps itself alive through the frame of `_asyncio.Future.result()`/`__await__`
  (the accelerator's Future is Python code embedded in crates/runtime/src/modules/_asyncio.rs,
  so its frame sits in the exception's traceback holding `self` -> the future -> the exception).
  CPython's C future has no such frame.  Fix belongs in _asyncio.rs (clear `self`/locals
  before raising), out of scope here.
- Kept because pinned: `SSLProtocol(...)` signature incl. `context`, `_app_transport`,
  `_app_protocol`, `_transport`, `_sslobj`, `_incoming`, `_outgoing`, `_get_extra_info`,
  `_fatal_error`, `_app_reading_paused`, `_write_backlog`/`_write_buffer_size`
  (`_test__append_write_backlog`), `_start_shutdown`, `_abort`, `pause_writing`/`resume_writing`,
  `_SSLProtocolTransport` with `_ssl_protocol`/`_closed`, `_create_transport_context`,
  `add_flowcontrol_defaults`, `SSLAgainErrors`, the two state enums, the hang1 drain bound in
  `_on_shutdown_complete`, every logged/raised message.
- New structure: one `_advance()` state dispatch (used for incoming data and after
  resume_reading), `_NEXT_STATES` successor table for `_set_state`, `_handshake_failed`,
  `_cancel_timers`, `_arm_shutdown_timer`, `_notify_app_writing`, `_schedule_app`, reading via
  `_read_into_app_buffer`/`_read_and_deliver` returning whether the stream is still open,
  `_checked_timeout`.
- Behaviour changes (all tests equal): a BufferedProtocol whose get_buffer() returns a
  bytearray now gets every decrypted chunk (reads go through a memoryview of the buffer; the
  old code sliced the bytearray, so a second read in one round went into a copy); EOF during
  the handshake reports a ConnectionResetError instance.
