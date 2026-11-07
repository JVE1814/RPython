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

## Environment-only failures (also fail on the baseline here)
* test_urllib (2): the session's https_proxy makes the https case tunnel through a proxy;
  passes with the proxy variables unset.
* test_socket (2): ThreadedVSOCKSocketStreamTest — this VM has /dev/vsock but no loopback
  VSOCK transport (ENODEV).
