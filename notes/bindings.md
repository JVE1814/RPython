# Workstream "bindings": _ctypes, _sqlite3, _ssl (system C libraries, DECISIONS D20)

## Layout
* New crate `crates/bindings` (`rpy-bindings`, depends on `rpy-runtime` + `libc`). The launcher
  calls `rpy_bindings::register()` before `rpy_runtime::initialize()`, which registers the
  builtin modules `_ctypes`, `_sqlite3` (and `_ssl`). Keeping the bindings out of the runtime
  crate means editing them only rebuilds this crate + the link step.
* All three C libraries are **dlopen'ed at runtime** (`dl.rs`, `dyn_lib!` macro): libffi.so.8,
  libsqlite3.so.0, libssl.so.3/libcrypto.so.3. No -dev packages or build-time linking needed.
  Declarations are written by hand from the documented C APIs (`*/ffi.rs`, `ctypes/libffi.rs`).
* `capi.rs`: extern declarations of the exported C API (PyLong_*, PyErr_*, PyEval_SaveThread,
  PyGILState_Ensure, ...). The bindings mix the runtime's Rust API and the C API.

## _ctypes (crates/bindings/src/ctypes, Lib/ctypes)
* Every ctypes type is a heap type whose metaclass derives from `_ctypes.CType_Type`; the
  metaclass instances are one pointer larger than `PyHeapTypeObject` and that slot holds the
  type's `StgInfo` (size/align/length/libffi type/proto/flags/PEP 3118 format/shape/
  `__pointer_type__`). StgInfo is created by the metaclass `__init__` ("already initialized"
  SystemError on a second call; types created by `Meta.__new__` alone are "abstract class").
* The base classes (`Structure`, `Union`, `Array`, `_Pointer`, `_SimpleCData`, `CFuncPtr`) are
  static types whose `ob_type` is their metaclass; they never have StgInfo. `_CData` is the
  common static base (buffer protocol, `_objects`, `_b_base_`, `__reduce__`).
* `CDataObject` layout: b_ptr, b_needsfree, b_base, b_size, b_length, b_index, b_objects,
  weakref list, 16 bytes inline storage. Keep-alive (`_objects`) follows the documented
  semantics (root container dict keyed "index:index...", single object for simple types).
* Structure/union layout is computed in Python by `Lib/ctypes/_layout.py` (own design:
  `get_layout(cls, fields, is_struct, base, base_size, base_align, base_length)` returning
  CField objects + size/align/format); gcc-sysv and ms bit-field rules, `_pack_`, `_align_`,
  `_swappedbytes_`, `_anonymous_` handled in Rust after the layout.
* Calls: libffi `ffi_prep_cif`/`ffi_prep_cif_var` (variadic when more args than argtypes),
  `ffi_call` with the thread state detached (PyEval_SaveThread) unless FUNCFLAG_PYTHONAPI;
  callbacks are libffi closures; the trampoline attaches with PyGILState_Ensure. errno swapping
  for use_errno. paramflags (in/out/lcid), errcheck, `_check_retval_`, ArgumentError wrapping.
* `string_at`/`wstring_at`/`memoryview_at`/`cast` are Rust `extern "C"` functions exported as
  addresses (`_string_at_addr` ...) and wrapped with PYFUNCTYPE in Lib/ctypes, like the
  documented design; `memmove`/`memset` are libc's.
* `pythonapi = PyDLL(None)` resolves the C API from the executable (exported with
  --export-dynamic).
* Lib/ctypes: `__init__`, `_layout`, `_endian`, `util` (find_library via ldconfig/gcc/ld,
  `dllist()` via dl_iterate_phdr), `wintypes` (importable everywhere), `macholib/`.
* `_ctypes_test`: our own C helper library `tools/ctypes_test/_ctypes_test.c` (written from
  the test expectations; spec has no `_ctypes_test.c`) + `_ctypes_test_generated.c.h`, which is
  produced by running the spec's own generator `test_generated_structs.py` with OUR python.
  Build: `tools/ctypes_test/build.sh [OUTDIR=build/ext] [PYTHON]` and put OUTDIR on
  PYTHONPATH/EXTRA_PYTHONPATH (like `_testcapi` from tools/build_test_exts.sh). The coordinator
  may want to hook it into build_test_exts.sh.

### Status (debug build, per-module unittest runs)
All test_ctypes modules pass except:
* test_pep3118: `len(memoryview(c_int()))` must raise TypeError ("0-dim memory has no length");
  runtime `types/memoryview.rs::memory_length` returns 1 for ndim 0 (bytes workstream file, not
  changed here).
* test_values.test_frozentable: needs `_PyImport_FrozenBootstrap` (+ FrozenStdlib/Test) data
  symbols exported by the runtime (capi workstream).
* test_structures.test_packed_c_limits needs `_testcapi` on the path (environment).
* test_win32/test_win32_com_foreign_func/test_macholib: Windows/macOS only (skips).

## _sqlite3 (crates/bindings/src/sqlite, Lib/sqlite3)
* Connection (subclassable, re-initialisable, callable -> Statement, `(sql, /)` signature),
  Cursor, Row, Blob, PrepareProtocol, Statement (not instantiable); module functions connect
  (at most 1 positional argument), adapt, register_adapter/converter, complete_statement,
  enable_callback_tracebacks; all result/extended codes, authorizer codes, limits, DBCONFIG
  constants; exception hierarchy with `sqlite_errorcode`/`sqlite_errorname`.
* 3.12+ transaction semantics: `autocommit` (True/False/LEGACY_TRANSACTION_CONTROL) and legacy
  `isolation_level` implicit BEGIN before DML; statement LRU cache (`cached_statements`).
* Callbacks (functions, aggregates, window functions, collations, authorizer, progress, trace)
  run with PyGILState_Ensure; errors are reported through `sys.unraisablehook` only when
  `enable_callback_tracebacks(True)`. Hook contexts replaced during a call are kept alive
  ("retired") until the connection dies.
* blocking SQLite calls (prepare/step/open/backup/blob I/O) detach the thread state.
* Lib/sqlite3: `dbapi2` (deprecated default date/datetime adapters; tolerates a missing
  datetime module), `dump` (own iterdump implementation using SQLite's quote()), `__main__`
  (CLI on code.InteractiveConsole with _colorize prompts).
* Status: every test_sqlite3 module passes (test_dbapi needs `urllib.parse`, which master
  does not have yet — verified with a local stub). Not done: `SQLITE_KEYWORDS` (completion
  tests are skipped without readline anyway).

## _ssl (crates/bindings/src/ssl, Lib/ssl.py)
* Binds libssl.so.3/libcrypto.so.3 (OpenSSL 3.0.13 here). `_SSLContext` (protocols with the
  deprecation warnings, options, verify_mode/flags, check_hostname, min/max version,
  security_level, num_tickets, post_handshake_auth, sni_callback, keylog_filename,
  `_msg_callback`, `_host_flags`, set_ciphers/get_ciphers, `_set_alpn_protocols`,
  load_cert_chain incl. password str/bytes/callable, load_verify_locations incl. PEM/DER
  cadata, load_dh_params, set_default_verify_paths, cert_store_stats, get_ca_certs,
  set_ecdh_curve, session_stats, `_wrap_socket`, `_wrap_bio`), `_SSLSocket` (handshake, read
  into buffer, write, pending, getpeercert dict/DER, verified/unverified chains as
  `_ssl.Certificate`, cipher, shared_ciphers, version, ALPN, tls-unique channel binding,
  post-handshake auth, shutdown, session get/set, session_reused), MemoryBIO, SSLSession,
  Certificate, RAND_*, txt2obj/nid2obj, `_test_decode_cert` (subject/issuer RDNs,
  serial, validity, subjectAltName incl. IP/DirName/RID, OCSP, caIssuers, CRL DPs),
  get_default_verify_paths, constants, OPENSSL_VERSION*, exception hierarchy (immutable
  types, `str()` = message, `library`/`reason` from the OpenSSL error queue, mnemonics
  derived from the reason strings; SSLCertVerificationError with verify_code/message and
  the hostname/IP mismatch texts).
* Socket I/O uses the socket's fd (SSL_set_fd); timeouts and non-blocking mode follow the
  Python socket's `gettimeout()` with poll(), the thread state is released around OpenSSL
  calls and waits. SNI/keylog/msg callbacks re-attach with PyGILState_Ensure.
* Lib/ssl.py is written from the documented API (enums via `_convert_`, SSLContext
  properties, SSLSocket on socket.socket, SSLObject, create_default_context,
  `_create_unverified_context`, DER/PEM helpers, cert_time_to_seconds, get_server_certificate).
* Verified by hand: TLS 1.3 handshakes over MemoryBIO pairs and over socketpair() with a
  server thread, verification errors, PEM/DH error attributes. test_ssl itself cannot be
  loaded yet: it imports `http.client`/`urllib.request`, which master does not have.
* Not implemented: TLS-PSK callbacks (HAS_PSK False, the setters raise NotImplementedError),
  NPN (deprecated anyway), Windows certificate stores, kTLS, `_ssl.enum_*` (Windows).

## Decisions
* B1: bindings live in their own crate registered by the launcher (fast rebuilds; no runtime
  changes needed besides the launcher line).
* B2: system libraries are dlopen'ed (no link-time dependency); missing libraries make the
  module import fail with ImportError, like a CPython build without the extension.
* B3: `_hashlib` stays the compress workstream's own implementation (not switched to libcrypto).
