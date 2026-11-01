# web workstream notes

## Owned files
- `Lib/ipaddress.py`
- `Lib/urllib/` (`__init__`, `parse`, `request`, `response`, `error`, `robotparser`)
- `Lib/http/` (`__init__` HTTPStatus/HTTPMethod, `client`, `server`, `cookies`, `cookiejar`)
- `Lib/wsgiref/` (whole package)
- `crates/runtime/src/modules/_asyncio.rs` (+ registration line in `modules/mod.rs`)
- `Lib/asyncio/` (taken over from the net workstream): fixes all over, new
  `sslproto.py` (MemoryBIO state machine), `proactor_events.py`, `tools.py`, `__main__.py`.

All written from scratch (D19): only names, messages and behaviour observed by the
tests are matched.  `ftplib` was written by markup (theirs is in master); mine was dropped.

## Decisions
- `_asyncio` is a builtin module whose Future/Task/registry/running-loop code is Python
  source compiled at import (the `_types` mechanism).  It is an independent second
  implementation (so the C-variant tests run against it) with the C accelerator's
  observable contract: non-deletable `_asyncio_future_blocking`/`_log_traceback`,
  `_callbacks` returns a fresh list or None, uninitialized futures raise
  "Future object is not initialized." (but result()/exception()/done()/cancelled()
  work on the class defaults), module functions do not bind as methods, lazily named
  tasks.  Frames of `__await__`/`result()`/task wakeup never keep the future (and so a
  failed future's exception) alive through tracebacks (test_streams refcycle test).
  TODO (perf): move Future/Task hot paths into Rust.
- `urllib.parse`: 3.15 `missing_as_none=` / `keep_empty=`.  Result objects carry the
  `_keep_empty` flag in their `__dict__` (pickle state `(None, {'_keep_empty': True})`
  as in the 3.15 pickles of the tests).  Final result classes deliberately have no
  `__slots__`: the runtime does not give a `__dict__` to `class B(A, tuple):
  __slots__ = ()` when A has a dict (CPython does) — runtime bug, reported below.
- `urllib.robotparser` implements RFC 9309 (wildcards, `$`, longest match, Allow wins
  ties, percent-encoding normalisation with significant `?`, `=`, `&`).
- `http.server`: error responses always carry a status line (also for HTTP/0.9-style
  requests); 0.9 requests do not read headers; paths starting with `//` are reduced to
  one slash; If-Modified-Since compared with an own UTC date computation (no datetime).
  `extra_response_headers` (3.15) skip names already sent unless repeatable
  (Set-Cookie, Link, ...).
- `http.cookiejar`: own tokenizer-based date parsers (linear time, no datetime/calendar),
  insertion-ordered jar iteration, Mozilla magic matched ASCII-case-insensitively.
- asyncio REPL (`python -m asyncio`): loop in the main thread, console in a daemon
  thread, statements run as tasks in one shared contextvars context; uses `_pyrepl` when
  `_pyrepl.main.CAN_USE_PYREPL`, else `code.InteractiveConsole.interact`.
- ssl (D20): no `ssl` code here.  http.client/urllib.request/http.server/ftplib import
  ssl lazily/optionally (`HTTPSConnection`/`HTTPSHandler` only exist when `ssl` imports;
  `HTTPSServer` raises RuntimeError without ssl).  asyncio.sslproto is a complete
  MemoryBIO-based implementation, untested until `_ssl` lands.  Needs from bindings:
  `SSLContext.wrap_bio(incoming, outgoing, server_side=, server_hostname=)`,
  `MemoryBIO` (`write`, `read`, `write_eof`, `pending`, `eof`), `SSLObject.do_handshake`
  / `read(n)` / `write` / `unwrap` / `getpeercert` / `cipher` / `compression` /
  `version`, exceptions `SSLWantReadError`, `SSLWantWriteError`, `SSLZeroReturnError`,
  `SSLSyscallError`, `SSLCertVerificationError`, `CertificateError`; http.client needs
  `ssl._create_default_https_context()`, `SSLContext.set_alpn_protocols`,
  `.post_handshake_auth`, `.wrap_socket(sock, server_hostname=)`; http.server needs
  `create_default_context(Purpose.CLIENT_AUTH)`, `load_cert_chain(certfile, keyfile,
  password)`, `wrap_socket(server_side=True)`.

## Status (with throwaway out-of-git copies of deps not yet in master)
- test_ipaddress 212/212, test_urlparse 81 OK, test_urllib_response OK,
  test_http_cookies OK, test_http_cookiejar OK, test_robotparser OK,
  test_urllib2_localnet OK, test_httplib 151 OK (12 skipped: ssl), test_wsgiref 46 OK,
  test_urllib 106 OK, test_urllib2 85 (1 failure: mimetypes `;type=a`, see below),
  test_ftplib 96 OK (49 skipped: ssl, IPv6), test_httpservers 101 OK.
  asyncio REPL: all TestAsyncioREPL / test_asyncio_repl_* tests of test_repl pass.
- test_asyncio: C variants run against `_asyncio`; test_proactor_events 82 OK,
  test_sock_lowlevel 45 OK, test_tools 22 OK (with a stub `_remote_debugging` providing
  only the record types — see below).

## Blocked on other workstreams / unassigned
- `concurrent.futures`, `multiprocessing` (parallel): asyncio imports
  concurrent.futures; test_events/test_unix_events need multiprocessing.
- `array` (data): test_httplib/test_urllib2 import it.
- `_remote_debugging` (unassigned): test_asyncio.test_tools imports it at module level
  (only the structseq types LocationInfo/FrameInfo/CoroInfo/TaskInfo/AwaitedInfo are
  used there); `asyncio ps/pstree` need `RemoteUnwinder(pid).get_all_awaited_by()`.

## Runtime issues found (not my files)
- `types.FunctionType(module_code, globals)()` fails with "SystemError: no locals found
  when storing X" (a function whose code lacks CO_NEWLOCALS should use its globals as
  locals), and `eval()` of code compiled with PyCF_ALLOW_TOP_LEVEL_AWAIT does not return
  a coroutine ("yield from a non-generator frame").  The asyncio REPL works around both
  (sync statements via exec; statements with await recompiled from the AST into an
  `async def` whose assigned names are declared global).
- An exception propagating out of awaited coroutines gets every frame twice in its
  traceback ("line 427, in wait_for" twice): test_futures2.test_future_traceback.
- `decimal.getcontext()` is not contextvar-based although `decimal.HAVE_CONTEXTVAR` is
  True (a context changed inside `Context.run` leaks out): test_asyncio.test_context.
- Intermittent segfault when `gc.get_referrers()` runs while another thread executes
  (test_streams.test_open_connection_happy_eyeball_refcycles, ~1 in 3 runs).
- `class B(A, tuple): __slots__ = ()` with a dict-having A: instances get no __dict__.
- Re-raising a stored exception with `with_traceback(tb)` appears to extend the stored
  traceback chain (test_futures2.test_future_traceback counts 2 frames; Py and C
  variants alike).
- mimetypes (markup): `guess_type('ftp://h/baz.gif;type=a')` ignores the `;params` of
  the URL path (CPython gives image/gif).

## asyncio 3.15 behaviour implemented (from the tests)
- servers accept connections in the context captured when the Server was created
  (`_start_serving(..., context)`), transports run all their callbacks in their creation
  context (gh-140947 test), `call_soon_threadsafe()` returns `events._ThreadSafeHandle`,
  `writelines()` drops empty chunks, cancelled `sock_accept()` leaves the connection
  pending, subprocess `wait()` returns when the child exits even if a pipe stays open,
  a failing synchronous `client_connected_cb` is reported and closes the transport,
  `TaskGroup.cancel()`.

## Full test_asyncio (-j8, with parallel's concurrent/multiprocessing copies)
27/35 files OK, 2 Windows-only skipped.  Remaining failures: test_base_events
(`ssl_server_hostname_default` needs ssl), test_context (decimal not contextvar-based),
test_futures2 (doubled traceback frames, runtime), test_unix_events (fork/multiprocessing
manager tests from the parallel WIP: NameError in forked thread, ForkAwareLocal).
env_changed in test_events/test_streams: the helper HTTP server thread of
test_asyncio/utils.py reports "Invalid file descriptor: -1" (serve_forever on a closed
server socket) — not investigated.

## Next steps
- Investigate the `Invalid file descriptor: -1` thread exception (socketserver /
  selectors interaction when the server is closed while serving).
- Rerun test_asyncio/test_unix_events once concurrent.futures and multiprocessing are in
  master; test ssl paths (sslproto, start_tls, HTTPS, FTP_TLS) once `_ssl` lands.
- `_remote_debugging` for test_tools / `asyncio ps|pstree`.
- Move `_asyncio` hot paths to Rust.

## Missing / TODO
- asyncio `windows_events`/`windows_utils` (Windows only).
- Perf: `_asyncio` in Rust.
