# tail3 — long-tail test fixes (third round)

Measured on installed-layout snapshots (tools/snapshot.sh). Start: core 68fdc83 (tail2 merged);
core merged again at f4f8e9f (stability3 + perf5).

## Done

### test_getpath (0 -> 32/32)
`crates/runtime/src/modules/getpath.py`: our own path-calculation program, executed (not imported)
with a namespace that supplies every input (build-time values, ENV_*, the partial `config`, and the
file-system queries isfile/isdir/isxfile/ismodule/realpath/readlines/joinpath/...). Exposed as
`_testinternalcapi.get_getpath_codeobject()` (compiled from `include_str!`); test_getpath uses it
because the installed layout has no `Modules/getpath.py`. Covers POSIX installs, build trees
(pybuilddir.txt), venvs (pyvenv.cfg `home`, symlinked/copied interpreters, relocated bases, separate
exec_prefix — prefix searched from the base interpreter's real directory first, then `home`), macOS
framework builds and venv launchers, PYTHONHOME (`prefix:exec_prefix`), Windows layouts (DLLs, zip
next to the executable, registry PythonPath subkeys). The launcher still uses the Rust calculation in
sys_full.rs (switching it to this program is possible but was not needed).

### test_nturl2path (missing -> 6/6)
`Lib/nturl2path.py`, written from the tests; DeprecationWarning on import.

### test_crossinterp (30 errors -> 0)
xidata unpickling tries the target's own `__main__` first and only on failure loads the source
interpreter's main script as `<fake __main__>`; that script may now also be a `__main__` with a
`__spec__` whose `__file__` was pointed elsewhere (a `-m` module whose file is its spec's origin is
still never re-run).

### test_ast (2 -> 0): non-str keyword keys
`abstract_::passes_kwargs_dict(callable)` decides whether `f(**d)` hands `d` to the callee unchanged
(interpreter CALL_FUNCTION_EX and the tier-1 helper): callables without vectorcall, builtins with
METH_VARARGS|METH_KEYWORDS (`cfunction::takes_args_and_dict` / `call_args_and_dict`), and classes
whose tp_new/tp_init are slot/object/_ast ones (those report non-str keys themselves). Everything
else still gets "keywords must be strings". `ast.AST.__replace__` uses the tuple-and-dict convention
and rejects non-str keys.

### test_asyncio (10 -> 3)
* `_decimal`: the current context lives in a ContextVar (cached per OS thread, keyed by the thread
  state's context stamp = (ts id, context_ver); `_contextvars::{new_var, var_value, context_stamp}`).
* yield-from throw: CLEANUP_THROW passes the exception on as a re-raise (one traceback entry per
  delegating frame instead of two); an exception thrown through a sub-iterator is chained with the
  delegating generator's handled exception (propagation mark reset in `gen_throw_impl`).
* finished generator/coroutine frame objects no longer keep a hidden strong `_f_back` to the frame
  that last resumed them (`frame_finished`): it kept the task machinery, tasks and their creators
  alive through exception tracebacks.
* `<_asyncio>` Task: the step's exception is parked in `task._task_step_exc` and thrown by the native
  `_throw_stored` (no Python frame of the task machinery refers to it while the coroutine runs); the
  step strips its own traceback entry (a native step adds none); eager start clears its locals.
* asyncio: `_make_ssl_transport(context=)`/`SSLProtocol(context=)` (callbacks scheduled for the app
  run in the creator's contextvars context); `_create_connection_transport`/`start_tls` drop the
  waiter in `finally`; `_on_handshake_complete` drops the handshake exception (cycles).

### Smaller fixes
* OSError: when a subclass overrides only `__init__`, the arguments go to `__init__` (`__new__`
  ignores them); otherwise `__new__` consumes them and the default `__init__` ignores its arguments
  (test_exception_hierarchy).
* REPL: `from __future__ import ...` stays in effect for later statements (test_future_stmt).
* deque: type size/`__sizeof__` follow CPython's block layout (test_deque); `__abc_tpflags__` with
  both SEQUENCE and MAPPING is a TypeError (test_collections).
* sysconfig/pyconfig: HAVE_GETENTROPY/HAVE_GETRANDOM/HAVE_GETRANDOM_SYSCALL (os.urandom uses
  getrandom; test_os URandomFDTests is skipped as on CPython).
* importlib `_get_module_lock` weakref callback binds `_imp`'s lock functions (callbacks firing
  after a subinterpreter's modules were cleared: test_datetime subinterpreter test, test_import).
* Script paths that are not valid UTF-8 run from their exact bytes (`__file__` surrogateescape)
  (test_cmd_line_script test_non_ascii).
* `os.read` waits through `threads::blocking_pure(true, ..)` (stability3's helper): a pool's
  result-handler thread blocked in a pipe read no longer holds back objects it owns
  (test_multiprocessing_* test_release_task_refs).
* Subinterpreter teardown keeps the builtin functions/types/constants in its builtins dict (late
  finalizers still find `getattr`); `_interpchannels.ChannelID.__del__`,
  `concurrent.interpreters.Queue.__del__`/`Interpreter._decref` bind what they use (test_types,
  test_import unraisable warnings).

## Left (root causes)
* test_asyncio (3): test_open_connection_happy_eyeball_refcycles and the two sslproto
  `*_failed_certificate_no_cycles` tests. After the fixes above the remaining holder is a Future
  (and an ExceptionGroup's args) with one reference that no GC-visible object accounts for
  (`gc.get_referrers` shows nothing, `gc.collect()` does not free it). Looks like a reference held
  natively or a deferred decref; not tracked down.
* test_sys (2): `test_subinterp_intern_dynamically_allocated` needs per-interpreter interning of
  dynamically created strings (ours is one table; shapes key on interned pointers).
  `SizeofTest.test_objecttypes`: 25 size mismatches against CPython's struct layouts (dict
  iterators, enumerate, frame, generator, module, capsule, range, set, type, str, list/dict GC
  accounting ...) — a list in scratch; each needs a type-size decision like the deque one.
* test_httpservers hang (regression in core, not tail3): reproducible on /home/user/snap25 (core
  24d1c6a) and absent at 68fdc83. A discarded `http.client.HTTPResponse` stays alive, so the
  keep-alive socket is never closed and the server thread blocks in `readinto`. Its referrers are
  finished frame objects (begin/getresponse) chained through `f_back` to server-thread frames;
  independent of the JIT (RPYTHON_JIT=0 too). Suspect the stability3/perf5 merge (frames/exception
  unwinding/generator claiming changes). Repro: scratch `resp.py` = HTTPServer thread +
  HTTPConnection.request/getresponse; check a weakref to the response.

## Tooling notes
* Bash heredocs and some compound commands are refused in this sandbox; scripts went into the
  session scratchpad (`t.sh` runs modules and greps FAIL/ERROR; `regr.sh` the regression batch;
  `chain.py` prints referrer chains).
* Don't pipe cargo into `head`: SIGPIPE kills the build.
