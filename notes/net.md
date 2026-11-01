# net workstream notes

## Owned files
- `crates/runtime/src/modules/_socket/{mod,addr,netdb,consts}.rs` — `_socket`
- `crates/runtime/src/modules/select.rs` — `select` (select/poll/epoll); single
  implementation (the earlier `system` version was replaced at merge time;
  `select::init_module` kept as the entry point used by sys_full).
- `crates/runtime/src/modules/_posixsubprocess.rs` — `fork_exec`; osio's duplicate
  `posixsubprocess.rs` was deleted at merge time (one implementation).
- `Lib/socket.py`, `Lib/selectors.py`, `Lib/subprocess.py`, `Lib/socketserver.py`
- `Lib/asyncio/` (whole package)

## Clean-room (D19)
socket.py, selectors.py, subprocess.py and the whole asyncio package were rewritten
after rule 9: structure, code, comments and docstrings are ours; only names and
behaviour observed by the tests are matched (private attribute names that tests poke,
error messages, repr formats, coroutine names visible in call-graph tests).

## Design decisions
- `_socket.socket` keeps CPython's timeout state machine: timeout -1 = blocking,
  0 = non-blocking, >0 = O_NONBLOCK + poll() with a deadline.  Every blocking syscall
  runs inside `threads::blocking`; EINTR -> `signal::check_signals()` then retry with the
  remaining time (PEP 475).  `connect()` on EINTR waits for completion like CPython.
- Error report protocol between `_posixsubprocess` and subprocess.py:
  `b"Name:hexerrno:msg"` ("noexec", "noexec:chdir" markers) — this is what
  test_subprocess writes in its mocks, so it is the observable contract.
- `subprocess._USE_POSIX_SPAWN = False`: fork_exec reports every failure precisely;
  `Popen._wait_pidfd` / `_wait_kqueue` implement the 3.15 fast timed wait
  (`_CAN_USE_PIDFD_OPEN`).
- asyncio child watchers: `_PidfdChildWatcher` reaps in the loop thread when the pidfd
  becomes readable; `_ThreadedChildWatcher` threads only observe the exit
  (`waitid(WNOWAIT)`) and the reaping + publication happen in the loop thread, so a pid
  is never recycled while asyncio may still signal it (test_subprocess gh-127049 tests).
- asyncio imports `concurrent.futures` lazily (only executors / run_coroutine_threadsafe
  need it) because the module does not exist in master yet.
- ssl: there is no OpenSSL binding, so `_ssl`/`ssl` stay absent (tests skip like on a
  CPython built without ssl).  `asyncio.sslproto.SSLProtocol` raises RuntimeError.
- `_asyncio` (C accelerator) is not implemented: the `_c_*` aliases are only defined when
  it exists, so the C-variant test classes skip.
- `select.select` uses real select(2) (FD_SETSIZE check -> ValueError like CPython).

## Status
- test_epoll: 11/11 OK; test_poll: OK (2 skipped); test_socketserver: 28/28 OK
  (`-u network`); test_select: 5/6 (disallow-instantiation, see below).
- test_socket, test_selectors, test_subprocess, test_asyncio: blocked on
  `unittest.mock` (not in master); test_asyncio additionally on `concurrent.futures`.
- Smoke-tested: TCP/UDP/UNIX sockets, timeouts, sendmsg/recvmsg + SCM_RIGHTS,
  getaddrinfo/getnameinfo, socketpair, makefile, sendfile; subprocess run/communicate/
  timeouts/errors/preexec_fn; asyncio gather/TaskGroup/timeouts/locks/queues/streams
  server+client/subprocess/as_completed/wait.

## Runtime issues found (not in my files)
- `Py_TPFLAGS_DISALLOW_INSTANTIATION` is not honoured by type_ready/tp_new_wrapper:
  `type(select.poll()).__new__(...)` gives "object.__new__(poll) is not safe" instead of
  "cannot create 'select.poll' instances" (test_select.test_disallow_instantiation).
- Segfault when one thread does `self[:] = [...]` on a list subclass while another
  thread reads it (seen in socketserver; worked around with a lock-protected class).
- `builtins.sentinel` (3.15) missing: test.support.os_helper fails to import in some
  paths; `array` module missing (socket.send_fds/recv_fds need it).

## Missing / TODO
- `_asyncio` Rust accelerator; asyncio proactor_events/windows_*; asyncio `__main__`
  REPL and `tools`.
- ipaddress, urllib (parse/request/response/error/robotparser), http
  (client/server/cookies/cookiejar) — not started.
- AF_TIPC / AF_BLUETOOTH addresses (constants for bluetooth intentionally absent so the
  tests skip), `socket.sendmsg_afalg` untested (no AF_ALG in the sandbox).
