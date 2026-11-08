# cleanroom7 — continuation of the D19 audit (see notes/cleanroom1..6.md)

## Method
- As in cleanroom5/6: a recalled module is rewritten from its public names, the documented
  behaviour, the spec tests and a grep of Lib/, crates/ and spec/ for every private name
  other code or the tests use.  Comments, docstrings and structure are new; messages are
  kept only where tests (or other modules) pin them, or where they are plain behaviour.
- Old files (base 4e3f92a, = core after `git merge core`) kept in an untracked
  `.scratch7/old/`.  Two installed snapshots of the same binary: target/cleanroom7/snap
  (old files) and target/cleanroom7/snapnew (new files), so "before -> after" runs and
  benchmarks were interleaved without swapping files.  Scripts: `.scratch7/rt.sh`
  (regrtest -j4 on a snapshot), `sync_new.sh`, `abs.sh` (A/B a script), `bench.sh`
  (pyperformance through tools/bench/run_bench.sh), fuzzers `fuzz_mp.py`, `fuzz_mime.py`,
  `fuzz_mime3.py`, `fuzz_fold.py`.
- None of the rewritten files is frozen into the start-up image (D30: frozen.rs lists no
  multiprocessing/concurrent/email/xml module).  The binary was rebuilt only for the merge.
- The box was loaded (load 5-15); timing differences under ~15% are noise.

## Rewritten

### multiprocessing: util, process, context, synchronize, reduction, spawn, popen_fork,
### popen_forkserver, popen_spawn_posix, popen_spawn_win32, forkserver, resource_tracker,
### resource_sharer, heap, sharedctypes, shared_memory, dummy/__init__, dummy/connection
- Verdict (old): CPython's structure and text throughout (Finalize/_run_finalizers,
  _exit_function, BaseProcess._bootstrap, SemLock/Condition.notify, the fork server loop,
  Heap's free lists, ShareableList's layout and helper names, ...).
- Tests (before -> after, same binary): test_multiprocessing_fork/forkserver/spawn (all
  parts), test_concurrent_futures, test_multiprocessing_main_handling, test_genericalias,
  test_free_threading: 81 files, 2,082 tests, only test_concurrent_futures.test_init fails
  -> identical.  test_asyncio, test_compileall, test_fcntl, test_importlib, test_logging,
  test_memoryview, test_re, test_socket, test_audit, test___all__, test_venv: identical
  (test_importlib 6 errors and test_socket fail in both).
- Kept because pinned: util (`_finalizer_registry`, `_afterfork_registry`,
  `_run_finalizers`, `_exit_function` with default-bound arguments, `_cleanup_tests`,
  `_SUN_PATH_MAX`, `abstract_sockets_supported`, `Finalize` + messages,
  `ForkAwareThreadLock/Local`, `spawnv_passfds`, `close_fds`, `_close_stdin`,
  `_flush_std_streams`, `_args_from_interpreter_flags`, `get_temp_dir` fallbacks, stacklevel
  of the log helpers (test_filename)); process (`_dangling`, `_children`,
  `_current_process`, `_parent_process`, `_exitcode_to_name`, `ORIGINAL_DIR`,
  `_bootstrap(parent_sentinel)`, `_after_fork`, `_identity`, `_config`, `_popen`,
  `_sentinel`, `_kwargs` copy, AuthenticationString, the repr format); context
  (`_default_context`, `_force_start_method`, lazily chosen default start method — no
  tempfile/shutil/subprocess import at import time, `get_spawning_popen`,
  `set_spawning_popen`, `assert_spawning`, the `reduction` module attribute and the
  `reducer` property, class names `ForkProcess`/`SpawnProcess`/`ForkServerProcess`/
  `*Context`); synchronize (`SemLock._semlock`, `_make_name`, `_cleanup`,
  `SEM_VALUE_MAX`, Condition's `_sleeping_count/_woken_count/_wait_semaphore` counting
  scheme, every repr, the fork-context RuntimeError); reduction (`ForkingPickler` +
  `_extra_reducers`, `register`, `dump`, `sendfds/recvfds` wire format, `DupFd`,
  `HAVE_SEND_HANDLE`, `AbstractReducer`); spawn (`old_main_modules`, `spawn_main`,
  `_main`, `get_preparation_data` keys, `get_command_line`, `import_main_path`);
  forkserver (`_forkserver` attributes `_forkserver_pid/_address/_authkey/_alive_fd`,
  `_inherited_fds`, `_stop`, `_handle_preload(preload, main_path, sys_path, on_error,
  sys_argv)` and its warning texts, the request protocol, the hang1 settings pipe);
  resource_tracker (`_resource_tracker` with `_fd/_pid/_exitcode/_waitpid_timed_out/
  _use_simple_format/_lock/_stop/_check_alive`, `_CLEANUP_FUNCS`, both message formats,
  exit codes, warning texts); heap (`_arenas`, `_allocated_blocks`, `_len_to_seq`,
  `_pending_free_blocks`, `_n_mallocs/_n_frees`, `_lock`,
  `_DISCARD_FREE_SPACE_LARGER_THAN`); shared_memory (`_USE_POSIX`, `_make_filename`,
  `_SHM_NAME_PREFIX`, SharedMemory class attributes incl. `_flags` for subclasses,
  ShareableList `format` strings and `_get_packing_format`); sharedctypes and dummy APIs.
- New structure (examples): one `_level_logger` factory for the five log helpers;
  `Finalize._forget`; `_choose_temp_base`; BaseProcess `_signal_child`, `_check_in_parent`,
  `_describe_state`, `_exit_status`; contexts share `_MethodProcess` (a `_popen_module`
  name per start method); SemLock `_create` classmethod, `_foreign_holder`,
  `_holder_description`, Event `_peek`; spawn's preparation applied from a
  `_PREPARE_STEPS` table, `_how_to_load_main`, `_path_for_child`; popen_spawn_posix
  `_pickle_for_child` (shared with popen_forkserver); forkserver `_authenticate`,
  `_send_request`, `_startup_extras`, `_spawn_server`, `_read_settings`,
  `_receive_request`, `_serve_request`; resource tracker `_encode_message/_decode_message`,
  `_ensure_running_locked`, `_reap`, `_remove_leftovers`; heap free space indexed by
  `_sizes` + `_free_at_start/_free_at_end` with `_index_free/_unindex_free/
  _take_best_fit/_release_space`; ShareableList keeps one descriptor per slot (capacity,
  format, kind) in front of the data instead of four parallel tables.
- Differential fuzzing (`fuzz_mp.py`, 4 seeds): 60 random malloc/free sequences per seed
  through the old and the new Heap (block positions, free/used layout, arena sizes,
  counters), 3,000 random names x 2 formats through both trackers' encoders/decoders
  (cross-decoded), 150 random ShareableLists with random assignments and re-attachment by
  name: 0 differences apart from the deliberate changes below.
- Behaviour changes: a nested resource-tracker call (a semaphore finalizer run by the GC
  while the same thread is inside the tracker) no longer raises ReentrantCallError and
  loses the message — it is written directly, or queued until the tracker is running
  (cleanroom6 saw this as a spawn "env changed"); the tracker exits 3 on bad input even
  when a cleanup also failed (was 2); ShareableList rejects out-of-range negative indexes
  (the old one wrote index -n-1 into the last slot), and sizes a str slot so that its
  encoded value always fits (the old one truncated non-ASCII initial strings silently);
  SharedMemory unlinks a block it created if registering it with the tracker fails;
  `_check_not_importing_main`'s message has new wording; dropped private helpers nothing
  uses (`_platform_supports_abstract_sockets`, `_remove_temp_dir`, `_get_base_temp_dir`,
  `_log`, `_reducer_module`, `_tls`, `_tempdir_is_writable`, `_choose_default_method`,
  `_random_suffix`, `_owner_name`, `_reduce_partial/_rebuild_partial` (never registered),
  `_install_main`, `_fork_state` (now `_keep_tracker_fd_in_child()`), the old Heap and
  ShareableList internals).
- Speed (`bench_mp.py`, s, old vs new, 2 rounds): 8 fork children 0.039/0.026 vs
  0.044/0.035, 8 spawn 0.58/0.34 vs 0.55/0.57, 8 forkserver 0.042/0.053 vs 0.035/0.044,
  40k lock ops 0.007/0.011 vs 0.008/0.007, Value 0.015/0.016 vs 0.016/0.015, 3000 heap
  blocks 0.017/0.017 vs 0.018/0.025, ShareableList 0.015/0.016 vs 0.009/0.009, Event
  equal.  pyperformance concurrent_imap: 0.053/0.020/0.040 vs 0.032/0.037/0.028 (noise).

### concurrent/futures: process.py, thread.py, _base.py
- Verdict (old): CPython's layout and method names (`add_call_item_to_queue`,
  `wait_result_broken_or_wakeup`, `process_result_item`, the waiter class hierarchy,
  `_yield_finished_futures`, ...), comments reworded.
- Tests (before -> after): test_concurrent_futures (11 files, 580 tests), test_genericalias,
  test_compileall, test_asyncio, test_importlib, test_logging, test_free_threading: same
  (test_concurrent_futures.test_init and test_importlib fail in both; test_asyncio
  test_events "env changed" in both).
- Kept because pinned: `_ThreadWakeup` (looked up at construction: tests patch it),
  `_ExecutorManagerThread` with `run` (patched), `_spawn_worker` (patched),
  `_check_system_limits`, `_RemoteTraceback.tb`, `_TERMINATE/_KILL`, executor attributes
  `_processes`, `_executor_manager_thread`, `_executor_manager_thread_wakeup`,
  `_call_queue`, `_result_queue`, `_mp_context`, `_shutdown_lock`, `_broken`,
  `_force_shutting_down`, `_max_workers`, `_force_shutdown`, the messages the tests
  match ("terminated abruptly with exit code", "garbage collected", "forcefully shut
  down", "could not be started", argument errors); thread.py `WorkerContext.prepare`
  protocol (interpreter.py), `prepare_context`, `_counter`, `BROKEN`, `_WorkItem`,
  `_threads_queues`, `_global_shutdown_lock`, thread names; _base.py state constants,
  `Future._state/_result/_exception/_condition/_waiters/_get_snapshot`, waiter methods
  `add_result/add_exception/add_cancelled`, `DoneAndNotDoneFutures`, messages.
- New structure: manager thread = `_feed_workers`, `_wait_for_event` (returns an event
  tag), `_deliver`, `_reap`, `_replace_retired_worker`, `_fail_pending`,
  `_begin_shutdown`, `_close_pool`, `terminate_broken`; own work records
  (`_PendingCall`, `_Task`, `_Outcome`, `_PicklableError`, `_CallQueue`), `_worker_main`,
  `_run_batch`/`_flatten_batches`, `_find_system_limit`; executor state renamed
  (`_spawn_on_demand`, `_idle_workers`, `_pending`, `_queued_ids`, `_next_id`,
  `_shutting_down`, `_cancel_on_shutdown`).  thread.py: `_serve`, `_should_stop`,
  `_drain`.  _base.py: one `_Waiter(wake_when, outstanding)` instead of four classes,
  `_attach_waiter/_detach_waiter`, `_split_settled`, `_already_satisfied`, `_hand_out`,
  `Future._wait_until_done/_outcome/_finish/_run_callback`.
- Pitfall found: a `threading.Thread` subclass must not define `_handle` (it is the
  thread's handle attribute) — the first draft hung every pool.
- Speed (s, old vs new, 3 rounds): ProcessPoolExecutor map+submit forkserver
  0.095/0.116/0.086 vs 0.101/0.125/0.061, spawn equal, ThreadPoolExecutor 40k calls
  0.35/0.47/0.36 vs 0.44/0.42/0.40 (noise band), Future create/set/result loop
  0.088/0.080/0.083 vs 0.086/0.056/0.080.

### email/_header_value_parser.py: the MIME half and the folding helpers
- Rewritten: parse_mime_version, get_invalid_parameter, get_section, get_value,
  get_parameter (split into `_parameter_markers`, `_looks_encoded`,
  `_open_quoted_extended`, `_rfc2231_prefix`, `_raw_quoted_contents`),
  parse_mime_parameters (+ `_recover_parameter`), `_find_mime_parameters`,
  parse_content_type_header / parse_content_disposition_header (shared
  `_leading_token`, `_trailing_parameters`), parse_content_transfer_encoding_header,
  helpers `_scan`, `_absorb_until_semicolon`, `_mime_version_number`;
  `_fold_as_ew` (`_start_ew_run`, `_longest_ew`, `_ew_charset`) and
  `_fold_mime_parameters` (`_parameter_spelling`, `_rfc2231_continuations`).  The small
  lexical getters (`_get_run`, get_ttext/attrtext/token/attribute, `_get_cfws_wrapped`)
  were already an earlier agent's and are kept.
- Tests: test_email (incl. test__header_value_parser), test_smtplib, test_mailbox,
  test_imaplib, test_http_cookies, test_urllib2, test_httplib, test_logging: 3,088 tests
  pass before and after.
- Differential check: 40,000 random values (MIME and generic alphabets) x 15 functions
  plus 20,000 values built from realistic RFC 2231 parameters: parse tree (`ppstr()`),
  str, value, all defects, every token's instance attributes, rest, `.params`: 0
  differences except the IndexError fix below.  Folding: 14,000 random headers
  (unstructured, address lists, content-type/disposition with long and non-ASCII
  parameters, four policies, max_line_length 20-100): identical output.
- Behaviour changes: a parameter ending in `*` at the end of the input (`name*`) raised
  IndexError out of parse_mime_parameters; now it is an invalid parameter with a defect;
  `_rfc2231_continuations` forces progress (one over-long section) where the old loop
  spun forever (no character fits even without the charset prefix).
- Speed (MIME headers, 5 rounds best): 0.18-0.20 s -> 0.15-0.16 s.

### xml/sax/xmlreader.py
- Rewritten in own wording; InputSource keeps its state in plain `_system_id` etc.
  (nothing used the name-mangled attributes), shared `_abstract()` for the methods a
  driver must provide, `AttributesNSImpl._name_for_qname`.  Pinned: handler slots
  `_cont_handler/_dtd_handler/_ent_handler/_err_handler` (expatreader, saxutils),
  `_bufsize`, `AttributesImpl._attrs`, `_qnames`, setter parameter names, messages.
- Tests: test_sax, test_minidom, test_pulldom, test_xml_dom_xmlbuilder, test_xml_etree(_c),
  test_xmlrpc, test_docxmlrpc, test_plistlib, test_pyexpat: 1,098 tests pass, = .
- xml/sax/handler.py reviewed: API declaration (no-op methods, feature/property names)
  with own short docstrings; nothing to restructure.

## Sampled (verdicts, no rewrite)
Already done by earlier workstreams (not re-checked): argparse, logging, inspect, tempfile,
shutil, unittest, traceback, enum, dataclasses, contextlib, collections, functools,
weakref, typing, pprint, difflib, glob, types, queue, posixpath, mailbox, zipfile, asyncio
core files, xml.dom, the rest of multiprocessing.
- smtplib.py: **recalled** (sendmail's control flow, quoteaddr/_addr_only/_fix_eols/
  quotedata as in CPython, comments reworded).  Next candidate (700 lines; test_smtplib,
  test_smtpnet).
- subprocess.py: mixed — own pieces (`_chunks` bookkeeping in `_communicate`, `_probe_pidfd`,
  `_OutputAlias`) but CPython-shaped prologues (stdin flush/close in `_communicate`,
  Popen argument handling).  Heavily pinned by test_subprocess; partial rewrite candidate.
- threading.py: CPython's algorithms (Condition waiter locks, Semaphore, Barrier) with
  reworded names/comments; very heavily pinned (test_threading, lock_tests).  Low value.
- socketserver.py: own (`_quit_requested`/`_loop_stopped`, `_Threads`/`_NoThreads` helpers).
- json (decoder/encoder/scanner): own helpers (`_hex_quad`, `_Writer`, `_float_text`,
  `_PyScanner`) around the pinned public names.  Own.
- pathlib: own helpers (`_scan_local`, `_file_uri`, `_percent_decode`, `_local_is_dir`,
  `_glob._translate_segment`).  Own.
- selectors.py: own (`_RegistrySelector`, `_PollStyleSelector`, `_works`).
- uuid.py: CPython's getnode helper names (`_ifconfig_getnode`, `_ip_getnode`, ...), all
  pinned by test_uuid; bodies not reviewed in detail.
- http/client.py: renames only (cleanroom3); not re-reviewed.
- csv.py, base64.py, gettext.py, locale.py, dis.py, ast.py, ipaddress.py, optparse.py,
  imaplib.py, ftplib.py, configparser.py, codecs.py, os.py: only the top-level name lists
  were sampled; most show own helper names (`_materialize`, `_PluralParser`,
  `_grouping_intervals`, `_Dumper`, `_first_matching`, `_checked_wrapcol`, ...).  Not
  verified line by line.

## Still recalled / not done
- smtplib.py (recalled), subprocess.py (partly), threading.py (algorithms, pinned).
- concurrent/futures/interpreter.py is small and own; `__init__.py` lazy loader own.

## Known failures (same before and after on the same binary)
- test_concurrent_futures.test_init `FailingInitializerResourcesTest.test_forkserver`:
  `_resource_tracker._exitcode` 1 != 0.  Cause: the test calls `_resource_tracker._stop()`
  right after the inner test runner returns, while the failing test case (and the
  `log_queue` it created, whose three semaphores are registered with the tracker) has not
  been freed yet: the objects are only reclaimed by the cyclic GC here, whereas CPython's
  reference counting frees them when `runner.run()` returns.  Not a multiprocessing bug;
  it needs the TestCase/runner objects to be freed promptly (runtime or unittest side).
- test_importlib (6 errors) and test_socket: pre-existing.
- Leftovers: my first broken multiprocessing draft left ~240 `psm_*`/`sem.mp-*` entries
  in /dev/shm (23:32-23:33, harmless random names; removing them in bulk was refused);
  the one that broke a test for everybody (`/dev/shm/a:b`) was removed.
