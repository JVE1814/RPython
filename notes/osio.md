# osio workstream notes (OS + I/O layer)

## Files
Rust (crates/runtime/src):
- `types/structseq.rs` — generic struct sequence types (shared helper, see below).
- `modules/posix/{mod,path,stat,fs,scandir,proc}.rs` — the `posix` module.
- `modules/errno.rs`, `modules/stat_mod.rs` (`_stat`), `modules/time.rs` (`time`).
- `modules/io/{mod,iobase,fileio,bytesio,stringio,bufferedio,textio}.rs` — `_io`.
- `modules/posixsubprocess.rs` — `_posixsubprocess.fork_exec`.
- `modules/bufview.rs` — RAII buffer-protocol access (`BufView::get(obj, writable)`).
- `threads.rs`: `threads::blocking(|| syscall)` marks the thread Blocked for the GC.
- `modules/sys.rs`: `init_stdio()` (called at the end of `initialize`) replaces the
  bootstrap writers by real `TextIOWrapper`s and sets `builtins.open = _io.open`;
  the bootstrap writers stay if `_io` fails (set RPY_DEBUG_STDIO=1 to see why).
Python (Lib/): `os.py`, `posixpath.py`, `genericpath.py`, `stat.py`, `io.py`,
`_pyio.py`, `_strptime.py`.

## structseq helper (for other workstreams: sys.flags, resource, pwd, grp, ...)
```rust
use crate::types::structseq::{self, Desc, Field};
const FIELDS: &[Field] = &[Field::new("a", "doc"), Field::unnamed("doc"), Field::new("hidden", "")];
let tp = structseq::new_type(&Desc { name: "mod.name", doc: "...", fields: FIELDS, n_in_sequence: 2 })?;
let obj = structseq::new_instance(tp, vec![v1, v2, v3]);   // all n_fields values
structseq::disallow_instantiation(tp);                       // like sys.flags
```
Layout is CPython's: tuple with `ob_size = n_sequence_fields`, hidden fields stored
after the visible items; named fields are read-only member descriptors; the type
dict holds n_fields / n_sequence_fields / n_unnamed_fields / __match_args__.
`__reduce__` and `__replace__` behave like CPython. The C API
(`PyStructSequence_NewType/InitType2/New/SetItem/GetItem`, `PyStructSequence_UnnamedField`)
is exported from structseq.rs.

## Decisions
- UTF-8 mode is the default (3.15): text I/O default encoding and "locale" resolve
  to "utf-8"; stdin/stdout use errors="surrogateescape", stderr "backslashreplace";
  stderr is always line buffered, stdout only on a tty; `-u` gives write_through.
- TextIOWrapper has built-in Rust codecs for utf-8 / latin-1 / ascii (all standard
  error handlers except namereplace); every other encoding/handler goes through the
  Python `codecs` module (`codecs.lookup(...).incrementaldecoder/encoder`).
- The tell() cookie layout is `pos | flags<<64 | bytes_to_feed<<128 |
  chars_to_skip<<192 | need_eof<<256` in both _io and _pyio.
- io objects all share an `IoHeader` (dict, weakref list, flags, a small owner-thread
  lock that raises "reentrant call inside ..." like CPython's buffered lock).
- BytesIO shares an exact `bytes` initial value copy-on-write.
- Buffered* calls raw methods through Python (so Python raw classes work) with fast
  paths when the raw object is an exact FileIO; when raw is not FileIO, readinto()
  is given a `bytearray` (memoryview over our buffer is not used).
- `_strptime` uses its own proleptic-Gregorian helpers (no datetime import) and
  implements the 3.15 rules: `%d` without a year is a ValueError, `%e` without a
  year a DeprecationWarning; `%D %F %T %n %t %:z` are supported.
- Blocking syscalls in posix go through `retry_eintr`, which wraps them in
  `threads::blocking`; `time.sleep` uses clock_nanosleep(TIMER_ABSTIME) + EINTR
  handling with signal checks.

## Missing / known gaps
- posix: statx, xattr functions, splice, preadv/pwritev, forkpty/login_tty,
  posix_openpt/grantpt/unlockpt/ptsname, timerfd_*, setns/unshare, getgrouplist,
  initgroups, sched_param pickling details, multi-threaded fork() DeprecationWarning,
  fork() does not reinitialise interpreter thread state (hook:
  `modules::posix::after_fork_child_hook`).
- _io: BufferedReader with a non-FileIO raw needs `bytearray(n)` (bytes workstream);
  BytesIO.getbuffer() needs `memoryview` (builtins); no `_WindowsConsoleIO`;
  non-blocking BufferedWriter partial-write accounting is approximate;
  `__sizeof__` values are approximations.
- time: no `time.clock_settime` permission handling beyond OSError; strftime
  passes ASCII runs to libc strftime (non-ASCII/surrogates/NUL are copied as is).
- Lib/os.py, io.py, _pyio.py import `_collections_abc` (collections workstream) and
  `abc`; `_pyio`/TextIOWrapper for non-builtin encodings need `codecs`; `_strptime`
  needs `re` (+ `enum`) and `locale`.
- Could not run the spec test modules yet (unittest / collections / enum / codecs
  not available in this branch); verified with ad-hoc scripts (see commits).
