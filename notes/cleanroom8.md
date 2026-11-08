# cleanroom8 — continuation of the D19 audit (see notes/cleanroom1..7.md)

## Method
- As in cleanroom5-7: a recalled module is rewritten from its public names, the documented
  behaviour, the spec tests and a grep of Lib/, crates/ and spec/ for every private name
  other code or the tests use.  Structure, comments and docstrings are new; messages are
  kept where tests or other modules pin them, or where they are plain behaviour.
- Old files (base = core after `git merge core`, b23a559; ftplib from 1824219) kept in an
  untracked `.scratch8/old/` during the work (deleted at the end: `git show b23a559:Lib/...`
  restores them for the fuzzers, which take the old and the new path as arguments).  Two installed snapshots of the same binary: target/cleanroom8/snap (old
  files) and target/cleanroom8/snapnew (new files) for interleaved before/after runs and
  benchmarks.  Scripts (untracked, `.scratch8/`): `rt.sh`/`rtlist.sh`/`both.sh` (regrtest on a
  snapshot), `sync_new.sh`, `ab.sh` (A/B a script), `bench.sh` (pyperformance through the
  shim, error files kept in .scratch8 instead of /tmp), `grepnames.sh`, fuzzers
  `fuzz_smtp.py`, `fuzz_subp.py`, `fuzz_uuid.py`, `fuzz_http.py`, `fuzz_ftp.py`, stress
  `stress_thr.py`, micro benchmarks `bench_*.py`.
- None of the rewritten files is frozen into the start-up image (D30: frozen.rs has no
  smtplib/subprocess/threading/uuid/http/ftplib entry; threading is not imported at start-up
  either).  The binary was rebuilt only for the final merge.
- The box was loaded (load 10-17 throughout): timing differences under ~20% are noise.

## Rewritten

### smtplib.py (recalled: sendmail's control flow, quoteaddr/_addr_only/_fix_eols/quotedata)
- Tests: test_smtplib 85 (pass), test_smtpnet (network, skipped), test_logging, test_email,
  test_mailbox: 2,578 tests, identical before/after.
- Kept because pinned: the exception classes and their `args` layout (assigned directly, so
  that OSError does not turn a reply code into errno and str() stays "(code, msg)"),
  `quoteaddr`, `quotedata`, `_MAXLINE`, `_have_cram_md5_support` (tests patch it), module
  global `socket` (tests swap in mock_socket), `SMTP` attributes and class defaults
  (`debuglevel`, `sock`, `file`, `helo_resp`, `ehlo_msg`, `ehlo_resp`, `does_esmtp`,
  `default_port`, `esmtp_features`, `command_encoding`, `local_hostname`, `source_address`,
  `_host`, `user`/`password`, `_auth_challenge_count`), `_get_socket`, `_print_debug`,
  `_rset`, lower-case "mail from:"/"rcpt to:", every asserted message ("prohibited newline
  characters", "Server AUTH mechanism infinite loop", "No suitable authentication method
  found.", the SMTPUTF8 texts), audit events.
- New structure: `_ehlo_features()` parser, `_envelope_of()` (Resent- handling), `_wire_body`
  /`_crlf_lines`/`_dot_stuff`, `_reply_line` + `getreply`, `_split_host_port`,
  `_guess_local_hostname`, `_option_suffix`, `_give_up` (421 -> close, else RSET),
  `_forget_greeting`, `_b64`.
- Differential fuzz (`fuzz_smtp.py`, 3 seeds x 2-3k scripted sessions: random replies,
  multi-line/garbled/overlong lines, EHLO feature sets, sendmail/send_message/login/auth/
  data/rcpt/docmd; plus 20k strings through quoteaddr/quotedata/_addr_only): 0 differences.
- Behaviour changes: debug level 2 always prints microseconds in the time stamp (str(time)
  dropped them when the value was 0, which the test regex would reject); the `__main__`
  demo is a new small function.
- Speed (sendmail+send_message x1500 over a fake socket): 0.81/0.75/0.76 s -> 0.73/0.73/0.82.

### subprocess.py (partly recalled: Popen argument handling, _communicate's prologue, run/call)
- Tests (before -> after): test_subprocess 366 (43 skipped) = ; with `-u cpu`
  test_support goes from 7 failures to 0 (see below).  Wide set of 114 test files that spawn
  processes (`.scratch8/subp_tests.txt`: test_subprocess, test_asyncio, test_cmd_line*,
  test_embed, test_faulthandler, test_signal, test_sys, test_os, test_regrtest, test_venv,
  test_concurrent_futures, test_compileall, test_unittest, test_pdb, test_logging, test_ctypes,
  test_tarfile, test_zipfile, ...): 13,091 tests, same failures before and after
  (test_audit, test_concurrent_futures.test_init, test_profiling).
- Kept because pinned: `_fork_exec` (positional layout, errpipe write end at index 13),
  `_USE_POSIX_SPAWN`, `_PopenSelector`, `_PIPE_BUF`, `_CAN_USE_PIDFD_OPEN`, `_CAN_USE_KQUEUE`,
  `_MAXIMUM_WAIT_TIMEOUT` (patched), `_active`, `_cleanup`, `_args_from_interpreter_flags`,
  `_optim_args_from_interpreter_flags`, `Popen._execute_child` (overridden/patched; its
  arguments are now our own), `_communicate` (patched), `_wait(timeout=)` (patched; the
  ^C grace wait is exactly `_wait(timeout=0.25)` once), `_wait_pidfd`/`_wait_kqueue`
  (patched), `_child_created`, `_communication_started`, `_stdin_write`, `poll()` being
  called by send_signal, every asserted message, `__all__` (test__all__).
- New structure: `_StdioPlan` (the six descriptors, what was opened for the child, closed
  once and idempotently on success and on failure), `_SpawnOptions`, `_identity()` for
  user/group/extra_groups, `_environment_block`, `_executable_candidates`, `_fd_above_stdio`,
  `_child_failure()` report decoder; one reaping primitive `_reap_locked(flags)` used by
  `_poll_nowait(lost_status)`, `_try_reap` and the blocking wait; `_kernel_waiter`;
  `_grace_after_interrupt`; communicate state in an `_Exchange` object (encoded input +
  offset + received chunks, survives TimeoutExpired); `Popen._os` class attribute for the
  finalizer path; exceptions share an `_output_alias()` property instead of a mixin.
- Behaviour changes: `_args_from_interpreter_flags()` passes every -X option through (the
  old list missed cpu_count, int_max_str_digits, lazy_imports, no_debug_ranges,
  pycache_prefix, warn_default_encoding, disable-remote-debug: test_support's
  `test_args_from_interpreter_flags`, a `-u cpu` test, failed 7 subtests); a malformed
  child failure report (non-ASCII exception name, non-hex errno for an OSError) becomes
  SubprocessError("Bad exception data from child: ...") instead of leaking a raw
  ValueError/UnicodeDecodeError; stderr=STDOUT with no stdout redirection and
  sys.__stdout__ None uses fd 1; private renames: `_internal_poll` -> `_poll_nowait`,
  `_waitpid_lock` -> `_reap_lock`, `_sigint_wait_secs` -> `_ctrl_c_grace`,
  `_handle_exitstatus` -> `_record_status`, `_decode_child_error` -> `_child_failure`,
  `_input/_input_offset/_chunks` -> `_exchange`, `_can_fork_exec` -> `_can_spawn`; dropped
  `_mswindows`, `_USE_VFORK`, `_HAVE_POSIX_SPAWN_CLOSEFROM`, `_OutputAlias`, `_reap`, `_as_id`,
  `_get_handles`, `_close_pipe_fds`, `_save_input`, `_check_timeout`, `_remaining_time`,
  `_translate_newlines`, `_get_devnull`, `_try_wait`, `_reap_nowait`, `_interrupted_wait`,
  `_abandon_streams`, `_resolve_ids`, `_probe_pidfd` (nothing in Lib/, crates/ or spec/ uses
  them).
- Differential fuzz (`fuzz_subp.py`): 20k list2cmdline sequences, 5k child failure reports,
  3k exception/CompletedProcess reprs, and 450 real runs with random stdin/stdout/stderr
  (PIPE/DEVNULL/STDOUT/inherit), text/encoding/bufsize, input up to 300 kB, timeouts, via
  run()/check_output()/Popen.communicate() (incl. resuming after TimeoutExpired): 0
  differences except two 50 ms-timeout races (process start-up under load) and the
  deliberate report-decoding change.
- Speed (s, old vs new, 2 rounds): 150x run(/bin/true) 0.60/0.52 vs 0.59/0.45,
  100x check_output 0.30/0.30 vs 0.31/0.28, 100x wait(timeout) 0.36/0.33 vs 0.28/0.34,
  16 MB through cat (communicate) min-of-15 0.042-0.068 vs 0.052-0.068 (noise; the I/O loop
  was inlined after a first draft looked ~20% slower).

### threading.py (CPython's algorithms with reworded names)
- Tests (before -> after): 185 test files (`.scratch8/thr_tests.txt`: test_threading,
  test_thread, test_threading_local, test_queue, test_faulthandler, test_concurrent_futures,
  test_multiprocessing_fork/forkserver/spawn (all parts), test_asyncio, test_logging,
  test_sched, test_free_threading, test_sys, test_external_inspection, test_signal,
  test_subprocess, test_capi, test_io, test_ssl, test_socket, test_importlib, ...): 18,500
  tests, identical (failures test_concurrent_futures.test_init, test_importlib,
  test_profiling, test_socket in both).
- Kept because pinned: everything in `__all__`, the deprecated aliases and their warning
  texts, `_dangling`, `_shutdown`, `_register_atexit` (+ "can't register atexit after
  shutdown"), `_active`, `_limbo`, `_start_joinable_thread` (patched),
  `_HAVE_THREAD_NATIVE_ID`, `_CRLock`, `_PyRLock`, `_DummyThread`, `_MainThread` (type names
  printed by the fork tests), `_after_fork` (module function), `_main_thread`,
  `_active_limbo_lock`; Thread's `_started`, `_handle` (subclasses must not use the name),
  `_ident`, `_name`, `_daemonic`, `_target/_args/_kwargs`, `_stderr` (read by
  `_thread._excepthook`), the frame chain run <- `_bootstrap_inner` <- `_bootstrap`
  (test_external_inspection, test_faulthandler), at most 3 threading.py frames under
  Event.wait()/Event.set() (test_faulthandler's regex); RLock protocol `_release_save`,
  `_acquire_restore`, `_is_owned`, `_recursion_count`, `_at_fork_reinit`; Condition
  `_waiters`; Event `_cond`; Barrier `_cond`, `_parties`, `_action`, `_timeout`, and its whole
  state in the two ints `_state`/`_count` (multiprocessing's Barrier keeps them in shared
  memory; `_state` 0 = gathering); Semaphore `_value`, BoundedSemaphore `_initial_value`;
  every repr; Timer's `interval`, `function`, `args`, `kwargs`, `finished`;
  serialize_iterator `_lock`/`_iterator`.
- New design: Condition.wait() decides "notified" by whether its gate is still queued
  once it holds the lock again, notify() pops gates (no double release, no RuntimeError
  dance), notify_all() does not go through notify(); Semaphore is a mutex + FIFO of sleeper
  gates with direct hand-off of released slots (no Condition), `_withdraw` settles a
  timed-out or interrupted sleeper; Barrier phases `_GATHERING/_PASSING/_CANCELLED/_BROKEN`
  driven by `wait_for` predicates (`_admits`, `_decided`) with `_complete_round`/`_leave`;
  `_FailureReporter` object instead of a per-thread closure; Timer.run uses the result of
  `finished.wait()`; `_after_fork` converts a surviving `_DummyThread` explicitly and resets
  the others through `Thread._reset_after_fork`; `_ForeignThreadSentinel` in a thread-local
  removes a dummy thread's registry entry when its thread ends.
- Behaviour changes: a Condition waiter whose timeout expired while a notify() picked it
  returns True (it was notified) instead of False with the notification silently consumed;
  Semaphore wakes sleepers in FIFO order and a newcomer can no longer take a slot that was
  released to a sleeper; a negative Semaphore timeout polls; Timer sets `finished` even when
  the function raises; renamed privates `Thread._invoke_excepthook` -> `_report_failure`,
  `Thread._after_fork` -> `_reset_after_fork`, `_DummyThreadRemover` ->
  `_ForeignThreadSentinel`, `_thread_shutdown` -> `_join_remaining_threads`, `_set_name` ->
  `_set_os_thread_name`; dropped `_LockType`, `_enumerate`, `_make_invoke_excepthook`.
- Stress (`stress_thr.py`: bounded semaphores with mixed timeouts and slot accounting,
  Condition producer/consumer with timeouts and no lost items, 800 barrier rounds, abort/
  reset cycles, Event wake-ups, queue.Queue MPMC): passes 3x on the new file and on the old.
- Speed (s, best of 3, old vs new, 2 rounds): Semaphore acquire/release x100k 0.151/0.156 vs
  0.049/0.038, BoundedSemaphore with x100k 0.179/0.170 vs 0.084/0.070, contended semaphore
  0.13/0.19 vs 0.09/0.10, Condition ping-pong 0.087/0.028 vs 0.032/0.032, Event cycle
  0.11/0.13 vs 0.09/0.12, Barrier 4x1500 0.074/0.033 vs 0.048/0.049, Queue MPMC 0.14/0.06 vs
  0.16/0.07, start/join x300 0.039/0.045 vs 0.019/0.021; pyperformance concurrent_imap
  0.057/0.069/0.076 vs 0.075/0.024/0.070 (bimodal noise).

### uuid.py (recalled: getnode helpers, generator bodies, main())
- Tests: test_uuid 120 (19 skipped) = ; test_cmd_line, test_os, test_free_threading.test_uuid
  in the final run.
- Kept because pinned: `_MAC_DELIM`, `_MAC_OMITS_LEADING_ZEROES`, `_get_command_stdout`
  (patched with a (command, args) function), `_find_mac_near_keyword(command, args,
  keywords, get_word_index)`, `_find_mac_under_heading(command, args, heading)`,
  `_parse_mac`, every `_*_getnode`, `_GETTERS`, `_OS_GETTERS`, `_node`, `_generate_time_safe`,
  `_has_stable_extractable_node`, `_UuidCreate`, `_last_timestamp`, `_last_timestamp_v6`,
  `_last_timestamp_v7`, `_last_counter_v7`, the call pattern of os.urandom (10 bytes for a new
  counter, 4 for a tail), random.getrandbits and time.time_ns, the pickle state dict, messages.
- New structure: `_with_version`, `_int_from_hex/_int_from_bytes/_int_from_fields`,
  `_swap_leading_fields`, variant from the top three bits, `_prefer_universal` shared by both
  finders (candidate generators), `_next_word`, `_platform_getters`, `_gregorian_ticks`
  shared by uuid1/uuid6, `_name_based` for uuid3/uuid5, `_fresh_v7_counter`, a new main().
- Differential fuzz (`fuzz_uuid.py`, 4 seeds x 20k constructor calls over all five
  spellings with corrupted inputs and versions, every property/str/repr/hash/state, 2k
  comparisons and state round trips, 2k name-based, 3k time-based with mocked
  time/urandom/random/last-state (urandom call lists compared), 4k fake tool outputs through
  both finders and _parse_mac (AIX and default), 300 getnode fallbacks): 0 differences.
- Behaviour changes: `_get_command_stdout(command, args)` takes one argument string (split on
  whitespace; every caller passes one word); dropped the module aliases `int_`/`bytes_`.
- Speed: equal (parse x2000 0.0023 vs 0.0022, str 0.0019 vs 0.0016, properties 0.009-0.012
  vs 0.008-0.009, uuid4/uuid7/uuid5 equal).

### http/client.py (recalled: method-for-method CPython's, renamed only by cleanroom3)
- Tests (before -> after): test_httplib, test_urllib2, test_urllib, test_urllib2_localnet,
  test_xmlrpc, test_httpservers, test_docxmlrpc, test_wsgiref, test_logging, test_ssl,
  test_asyncio.test_streams, test_robotparser, test_unittest: 2,411 tests, identical
  (test_urllib's two `test_url_host_with_*_rejected` errors fail with the old file too).
- Kept because pinned: `_MAXHEADERS`, `_MAXINTERIMRESPONSES`, `_MAXLINE`,
  `_create_https_context`, `HTTPConnection._get_content_length(body, method)` (urllib),
  `_http_vsn`/`_http_vsn_str` (test_httpservers changes them), `_tunnel_host/_port/_headers`,
  `_tunnel()`, `_buffer` (tests replace it with counting lists), `_output`, `_send_output`,
  `_send_request`, `_create_connection`, `_context`, `_method`, `_raw_proxy_headers`,
  `_validate_method/_path/_host`, `_encode_request`, `_is_textIO`, `_get_hostport`,
  `HTTPResponse.fp`, `_close_conn` (patched), `_read_status`, `_check_close`, `_safe_read`,
  `chunked`, `chunk_left`, `length`, `will_close`, `debuglevel` and its print formats
  ("reply:", "header:", "send:"), the sendall pattern (head in one call, then body pieces of
  blocksize), the state texts in the ImproperConnectionState exceptions, every message.
- New structure: header validation by two compiled patterns, `_header_line` shared by
  putheader and the CONNECT request, `_host_header_value`, `_bracketed`, `_without_zone_id`,
  `_host_bytes`, `_read_header_lines`/`_parse_header_lines`, `_blocks_of` (file bodies),
  `_forget_finished_response`; responses: `_decide_framing`, `_check_close` with one helper,
  `_no_body_left`, `_account` (identity bookkeeping shared by read/read1/readline),
  `_exactly`, `_chunk_size`, `_chunk_window`; connection state `_phase`/`_open_response`
  instead of name-mangled `__state`/`__response`.
- Differential fuzz (`fuzz_http.py`, 4 seeds, 3-6k each of: responses with interim 1xx,
  odd status lines, identity/length/EOF/chunked bodies with extensions, trailers, garbage and
  truncation, HEAD/204/304, HTTP/1.0 keep-alive, three buffer sizes, random sequences of
  read/readinto/read1/readline/peek/readlines; requests with every body type, framing
  headers, invalid headers/methods/URLs, IPv6/zone/IDNA hosts, HTTP/1.0, tunnels; CONNECT
  exchanges): 0 differences after fixing one draft bug (empty status line).
- Behaviour changes: an invalid chunk size raises IncompleteRead without chaining the
  ValueError; renamed/dropped privates nothing uses (`_strip_zone`, `_as_latin1`,
  `_valid_header_name`, `_bad_header_value`, `_collect_header_lines`, `_message_from_lines`,
  `_STATE_*`, `_NOT_YET_KNOWN` -> `_UNKNOWN`, `_wrap_ipv6(_str)`, `_read_readable`,
  `_get_chunk_left`, `_read_next_chunk_size`, `_read_and_discard_trailer`, `_read1_chunked`,
  `_peek_chunked`, `_safe_readinto`, `_body_done`).
- Speed (s, old vs new, 3 rounds, noisy): 1500 plain responses 0.15-0.22 vs 0.11-0.23,
  1500 chunked 0.20-0.30 vs 0.19-0.31, 1500 requests 0.21-0.33 vs 0.11-0.32; chunked
  read(37)/readline/read1 loops equal (0.010-0.014 / 0.0044 / 0.006).

### ftplib.py (recalled: CPython's control flow, lazily compiled reply regexes)
- Tests: test_ftplib (all classes incl. TLS), test_urllib2, test_urllib: identical (test_urllib
  as above).
- Kept because pinned: the public API and `__all__` (test__all__ forbids new public
  names), `MAXLINE`, `CRLF`, `B_CRLF`, `MSG_OOB`, `FTP_PORT`, `all_errors`, class defaults
  (`passiveserver`, `trust_server_pasv_ipv4_address`, `maxline`, ...), `af`, `lastresp`,
  `_SSLSocket`, `_prot_p`, `parse150/227/229/257`, `print_line`, `ftpcp`, messages.
- New structure: `_REPLY_FAILURES` table, `_start_transfer` (REST + 2xx-then-1xx rule)
  shared by both data-connection modes, `_check_abort_reply`, `_checked_line`, `_wrap` for
  TLS, login's follow-up loop, regexes compiled at import, parse257 over an iterator.
- Differential fuzz (`fuzz_ftp.py`, 2 seeds x 40k random reply strings through the four
  parsers and sanitize, 4k scripted control sessions with multi-line/garbled/overlong
  replies through sendcmd/voidcmd/login/rename/delete/cwd/size/mkd/pwd/rmd/acct/quit/abort):
  0 differences.
- Behaviour changes: none found.

## Sampled (verdicts, no rewrite)
- os.py: walk() is restructured, but fwalk's iterative action stack (`_FW_WALK/_FW_YIELD/
  _FW_CLOSE` + `_fwalk_step`) is CPython's design; makedirs/removedirs/renames close to it.
  Frozen in the start-up image (rebuild before snapshotting).  **Partly recalled.**
- codecs.py: StreamReader/StreamReaderWriter/StreamRecoder follow CPython method for method
  (readline's growing readsize, `__getattr__(self, name, getattr=getattr)`, the
  `__reduce_ex__` refusal).  Frozen.  **Recalled.**
- configparser.py: own helpers (`_split_option`, `_delimiter_at`, `_ValueLines`); the rest
  not reviewed line by line.
- base64.py, csv.py, ipaddress.py: own helpers around binascii/_csv (`_split_lines`,
  `_checked_wrapcol`, `_delimiter_candidate`, `_closing_quote`, `_v4_parse`, `_v6_parse`,
  `_format_integer`).  Look own.
- locale.py: mixed (`_grouping_intervals` own; `_replace_encoding`, `_append_modifier`,
  `_parse_localename`, `_build_localename`, `_strip_padding`, `_test` are CPython names, some
  pinned by test_locale); not reviewed in detail.
- dis.py, ast.py, gettext.py, optparse.py, imaplib.py: not reviewed beyond cleanroom7's
  name sampling.

## Still recalled / not done
- os.py (fwalk), codecs.py (stream classes) — both frozen; locale.py (partly);
  imaplib.py, optparse.py, dis.py, gettext.py, ast.py unreviewed.

## Final check (after `git merge core`, rebuilt, installed snapshot target/cleanroom8/final)
- tools/jit_tests/run.sh: all ok.  tools/stress/run.sh: 21/21 PASS.
- 230 test files (`.scratch8/final_tests.txt`: the subprocess and threading sets above plus
  test_httplib, test_urllib*, test_ftplib, test_smtplib, test_uuid, test_email, test_mailbox,
  test_robotparser, test___all__, ...): 24,808 tests; failed test_concurrent_futures.test_init,
  test_importlib, test_profiling, test_socket, test_urllib — the same five fail with core's
  own binary (target/core) on the same snapshot; "env changed": test_asyncio.test_events
  (test_utils HTTP-server race), test_gc, test_warnings (as before).  test_audit, which
  failed in the pre-merge before/after runs with both old and new files, passes after the
  merge.

## Known failures (same with the old files on the same binary)
- test_concurrent_futures.test_init (forkserver resource tracker exit code, see cleanroom7),
  test_audit, test_profiling, test_socket, test_importlib, test_urllib
  (`test_url_host_with_control_char_rejected`, `..._newline_header_injection_rejected`);
  "env changed": test_asyncio.test_events/test_streams (test_utils HTTP-server thread race),
  test_gc (unraisable AssertionError), test_warnings (filters modified).
- Leftovers: none created in /dev/shm by this workstream (the psm_*/sem.mp-* entries there
  date from cleanroom7, 23:32-23:33).  /tmp has test_python_worker_*/tmp* directories from
  concurrent regrtest runs of several agents that cannot be attributed; left alone.
