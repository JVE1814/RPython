# tail1 — long-tail test fixes

Measured on an installed-layout snapshot (tools/snapshot.sh). For quick turnaround a
snapshot can be refreshed in place by copying the new binary and the changed Lib files
(the test extensions only need rebuilding when spec/Modules changes).

## Root causes fixed (group 1)

* Implied `__text_signature__` was lost for every METH_NOARGS/METH_O method of types whose
  methods are marked self-locking (`METH_LOCKSELF*`: StringIO, BytesIO, buffered/text io,
  enumerate, reversed, OrderedDict, ...). `flags_text_signature` now masks those internal
  bits (test_inspect: 94 errors).
* `_testinternalcapi` gained the helpers the tests call: `identify_type_slot_wrappers`,
  `EncodeLocaleEx`/`DecodeLocaleEx`, the `_PyTime_*` rounding converters,
  `_PyTraceMalloc_GetTraceback`; `get_static_builtin_types` read `type.__subclasses__`
  through attribute lookup (unbound method on `type` itself).
* Static types created more than once: `sys` structseq types (float_info, int_info,
  hash_info, thread_info, UnraisableHookArgs) were recreated by every subinterpreter's sys;
  a second, unused `asyncgen_hooks` type lived in sys_full; the C-API placeholder
  `PyPickleBuffer_Type` duplicated `_pickle`'s PickleBuffer (now the same object, exported
  under the C name).
* PyCodec_IncrementalEncoder/Decoder indexed the CodecInfo tuple (whose 3rd/4th items are
  the stream classes).
* Failing getbuffer providers may leave junk in `view.obj`; it is reset so it is never
  released (crash in test_buffer).
* struct 'P' overflow raises struct.error; memoryview 'c' accepts bytearray like struct.
* `bytes * n` always returns a new object (empty results were the shared b"" singleton,
  invisible to tracemalloc).
* tracemalloc's PyTraceMalloc_Track records the caller's traceback when called with the
  thread state detached (`capi::pystate::detached_own_frame`).
* Module/class bodies running an inlined comprehension: `locals()` is a snapshot of the
  mapping plus the comprehension's (hidden) variables, `frame.f_locals` a live
  FrameLocalsProxy over both (hidden slots are visible while bound).
* `issubclass(x, A | B)`: no class check on x in the union (typing aliases).
* math.log/log2/log10: an object whose float conversion overflows is used through
  `__index__` (exact big-int path); acosh/asinh/atanh use libm (std's atanh lost digits
  near +-1); `0.0 ** -1` message "zero to a negative power".
* random: getrandbits accepts > 2**31 bits; randint indexes b before adding 1.
* `_asyncio` looks submodules up in sys.modules first (works while `sys.modules['asyncio']`
  is None, test_logging).
* Lib: locale.getdefaultlocale no longer deprecated (3.15); UserDict |/|= accept any mapping;
  UserString.index/rindex unwrap UserString; genericpath owns ALLOW_MISSING/ALL_BUT_LAST
  (pickle by name); gettext NullTranslations accept non-numbers with a DeprecationWarning;
  xml.is_valid_name/is_valid_text; py_object/tkinter.Event are generic; new clean-room
  `_android_support` and `_apple_support`.

## Root causes fixed (groups 2-4)

* `_abc` / `_py_abc` instancecheck: an instance whose `__class__` lookup fails is checked
  by its type; the type is held strongly while `__subclasscheck__` runs.
* `compile()` and eval/exec of source text raise the `compile` audit event.
* `marshal._loads_without_positions` ignored its request once the no_debug_ranges
  setting had been memoised (doctest tracebacks showed carets).
* Tokenizer: "detected at line N" counts the last line of input without a newline (eval
  mode); "unexpected EOF" after a line continuation points just after the backslash, and
  has no column when nothing precedes it (no caret line).
* `type.__repr__/__str__` returning a non-str: "X.__repr__() must return a str, not T".
* defaultdict: `__missing__` stores with setdefault semantics (a re-entrant factory's
  value wins); `dd | frozendict` keeps the defaultdict type.
* `sys.set_int_max_str_digits` is per interpreter (subinterpreters start from the
  configured value).
* Attribute caches: a type whose custom `mro()` contains a class that is not an ancestor
  through its bases gets no version tag (changes to that class would not invalidate it).
* `__classdictcell__` now holds the type's own dict (lazy bounds/aliases/annotations see
  later class attribute changes); type() refuses `__mro_entries__` bases before the
  metaclass calculation; `__class_getitem__ = None` means not subscriptable.
* Module struct sequences (time.struct_time, os.stat_result, ...) are mutable like the
  heap types they are in CPython; structseq(seq, dict=None) is a TypeError.
* Unpacking errors show "got N" only for dict/set (no user `__len__` call); list.sort
  drops key objects before checking for modification; bytearray.extend propagates
  length-hint errors; bytes %-format checks the argument count before '*' types;
  complex(real=<complex>) warns; SimpleQueue.__sizeof__ counts its ring buffer;
  _stat SF_SETTABLE/SF_SUPPORTED/SF_SYNTHETIC per platform.
* Running `python archive.zip/pkg/sub` (a directory inside a zip) runs its `__main__`.
* yield from: throwing a GeneratorExit *instance* closes the sub-iterator first;
  a sub-iterator whose `close` lookup fails is reported as unraisable.
* f-strings: an unpaired backslash before `{` is the invalid escape `\{` (kept, warns);
  replacement fields nest at most 3 deep inside format specs.
* REPL on a pipe: a blank line inside an open bracket / triple-quoted string continues
  the statement.
* Tracing: a backward jump inside one line no longer fires a line event unless it lands
  on the first instruction of that line (yield-from resumption); a for loop over a
  generator that returns a value reports StopIteration as a legacy 'exception' event.
* Lib: platform.libc_ver rewritten (musl, chunk-boundary safe); getpass passes the
  terminal's editing characters (term_ctrl_chars) and calls fallback_getpass without
  echo_char; textwrap does not leave a trailing space before a broken long word; shlex
  imports os/collections lazily; difflib HTML template starts with a newline; fractions
  "0=" fill groups zeros; statistics (p)stdev raise ValueError for inf/nan data;
  mimetypes ignores ";params" of URL path segments; pydoc.ttypager is a function; copy
  of recursive frozendicts; cmd completion with '!' and no do_shell; json.encoder
  docstring example.

## Left (root causes)
* test_re (4): re.DEBUG output needs the parse-tree dump and CPython's compiled-code
  disassembly; our sre compiles in Rust with its own code format.
* test_zoneinfo (1): CZoneInfoCacheTest expects the C ZoneInfo (no `_weak_cache` on the
  base class); there is no `_zoneinfo` accelerator, both test variants use the Python class.
* test_exception_group / test_pyexpat / test_atexit / test_repl (1 each): allocation
  failure injection (`_testcapi.set_nomemory`) is not honoured by the runtime allocator
  (allocation code is perf4's).  test_repl's other failure is asyncio (out of scope).
* test_pdb does not load: Lib/pdb.py lacks `_pyrepl_available` (not in this assignment).

## Environment-only failures (also fail on the baseline here)
* test_urllib (2): the session's https_proxy makes the https case tunnel through a proxy;
  passes with the proxy variables unset.
* test_socket (2): ThreadedVSOCKSocketStreamTest — this VM has /dev/vsock but no loopback
  VSOCK transport (ENODEV).
