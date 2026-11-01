# web workstream notes

## Owned files
- `Lib/ipaddress.py`
- `Lib/urllib/` (`__init__`, `parse`, `request`, `response`, `error`, `robotparser`)
- `Lib/http/` (`__init__` HTTPStatus/HTTPMethod, `client`, `server`, `cookies`)
- `Lib/wsgiref/` (whole package)
- `crates/runtime/src/modules/_asyncio.rs` (+ registration line in `modules/mod.rs`)
- `Lib/asyncio/` (taken over from the net workstream), networking modules.

All written from scratch (D19): only names, messages and behaviour observed by the
tests are matched.

## Decisions
- `_asyncio` is a builtin module whose Future/Task/registry/running-loop code is Python
  source compiled at import (the `_types` mechanism).  It is an independent second
  implementation (so the C-variant tests run against it) with the C accelerator's
  observable contract: non-deletable `_asyncio_future_blocking`/`_log_traceback`,
  `_callbacks` returns a fresh list or None, uninitialized futures raise
  "Future object is not initialized." (but result()/exception()/done()/cancelled()
  work on the class defaults), module functions do not bind as methods, lazily named
  tasks.  TODO (perf): move Future/Task hot paths into Rust.
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
- ssl (D20): no `ssl` code here.  http.client/urllib.request/http.server import ssl
  lazily/optionally exactly like CPython (`HTTPSConnection`/`HTTPSHandler` only exist
  when `ssl` imports; `HTTPSServer` raises RuntimeError without ssl).  For the
  bindings workstream: http.client needs `ssl._create_default_https_context()`,
  `SSLContext.set_alpn_protocols`, `.post_handshake_auth`, `.wrap_socket(sock,
  server_hostname=)`; http.server needs `create_default_context(Purpose.CLIENT_AUTH)`,
  `load_cert_chain(certfile, keyfile, password)`, `wrap_socket(server_side=True)`.

## Status (with throwaway stubs for missing deps, see below)
- test_ipaddress 212/212, test_urlparse 81 OK, test_urllib_response OK,
  test_http_cookies OK, test_robotparser OK, test_urllib2_localnet OK,
  test_httplib 151 OK (12 skipped: ssl), test_wsgiref 46 OK,
  test_urllib 106 (2 errors: ftplib missing), test_urllib2 85 (ftplib, array,
  mimetypes `;type=a` params), test_httpservers: all non-datetime tests pass.
- test_asyncio: runs with concurrent.futures from the parallel branch; C variants now
  run (test_tasks 1133 tests, test_futures 189 OK).

## Blocked on other workstreams (not in master yet)
- `email` (markup), `html`, `mimetypes` (markup): needed by http.client/server,
  urllib.request.  Tested with the markup branch copies.
- `datetime`, `calendar` (data workstream; absent everywhere): test_httpservers imports
  datetime.  My modules avoid both.
- `array` (data): test_httplib/test_urllib2 import it.
- `concurrent.futures`, `multiprocessing` (parallel): test_asyncio, test_events,
  test_unix_events.
- `ftplib`: nobody owns it; urllib FTP handler and test_urllib2 need it (not written yet).
- `_remote_debugging`: test_asyncio.test_tools imports it.

## Runtime issues found (not my files)
- `class B(A, tuple): __slots__ = ()` with a dict-having A: instances get no __dict__.
- Re-raising a stored exception with `with_traceback(tb)` appears to extend the stored
  traceback chain (test_futures2.test_future_traceback counts 2 frames; Py and C
  variants alike).
- mimetypes (markup branch): `guess_type('ftp://h/baz.gif;type=a')` ignores the
  `;params` of the URL path (CPython gives image/gif).

## Missing / TODO
- http.cookiejar (not written), ftplib, asyncio proactor_events/windows_*, asyncio
  `tools` and `__main__` REPL, proper MemoryBIO-based `asyncio.sslproto`.
