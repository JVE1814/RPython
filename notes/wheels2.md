# Workstream "wheels2": pandas/msgspec failures that only happen on our interpreter

Branch `worktree-agent-a8f51ac613dd8affe`. Reproducers: `tools/wheels/repro/*.py` (run with
the binary of a snapshot/venv; the pandas parts skip without pandas).

## Measuring
* The ours-only list of stability2 (2,320) compared a run *without* marker selection with a
  baseline run that used `-m "not slow and not network and not db and not single_cpu"`
  (5,820 deselected). 818 of the 2,320 were tests the baseline never ran (io/test_clipboard:
  needs pytest-qt's `qapp`; io/sas slow tests: data files are not in the wheel; network/db).
  Restricted to tests the baseline ran: **1,502** ours-only.
* Runs now use the same selection:
  `tools/wheels/run_numpy_tests.sh -k -o DIR -t pandas -T 9000 -m 6000000 -- -m "not slow and not network and not db and not single_cpu"`
  Per directory: /tmp/w2/runpd.sh PY SUBDIR (writes /tmp/w2/res/*.only = failures not in the
  baseline's failed list, among the tests the baseline ran).

## Root causes fixed
1. **PyState_FindModule** (capi_ext.rs): a single-phase module (PyModule_Create in PyInit) was
   never registered for its def, so PyState_FindModule returned NULL. pandas' ujson keeps
   Decimal/Series/DataFrame/Index types in its module state: every type check failed and the
   encoder recursed through dir(obj) ("Maximum recursion level reached"). ~700 tests
   (io/json, extension/*json*, io/test_compression json cases).
2. **zoneinfo `_tz_after.transitions(year)`** (Lib/zoneinfo/_zoneinfo.py): pandas 3 reads the
   pure-Python ZoneInfo internals (`_trans_utc`, `_ttinfos`, `_tti_before`, `_fixed_offset`,
   `_tz_after`) and the POSIX rule's `transitions(year)` -> (start, end) local-standard /
   local-daylight epoch seconds. Without it every date after the last stored transition (2007
   for US zones in tzdata's slim files) used the last offset: one hour off in winter.
   ~400 tests (arithmetic, tseries/offsets, indexes/datetimes, tslibs/test_timezones, ...).
3. **datetime private field names** (_datetime.rs + Lib/_pydatetime.py): the embedded pure
   methods read instance state through getsets named `_year`, `_hour`, `_days`, ... which a
   subclass shadows (pandas' Timestamp has its own `_year`: isoformat of out-of-range years
   spliced the wrong year). Renamed to `_dt_*`; the old timedelta instance-dict workaround is
   gone.
4. **datetime arithmetic with a timedelta subclass** dispatched to the subclass's operators
   (pandas' Timedelta works in ns: OutOfBoundsTimedelta for `datetime + Timedelta`). Now on
   the fields (`_td_us`). Added `datetime.__rsub__` (the C type's slot also serves the
   reflected call: `Timestamp - datetime` when Timestamp.__sub__ declines).
5. **datetime argument errors as in 3.15** (clinic-style): "datetime() missing required
   argument 'day' (pos 3)", "'float' object cannot be interpreted as an integer", values
   outside C int -> OverflowError (dateutil/pandas turn that into OutOfBoundsDatetime).
6. **3.15 message texts pandas checks**: slice indices ("must be integers or have an
   __index__ method"); operator errors use tp_name ('numpy.ndarray'; a PyType_FromSpec type
   shows its full spec name: tp_name_of now returns tp_name for heap types too).
7. **_strptime**: `TimeRE._TimeRE__seqToRE` (pandas' TimeRE subclass calls `self.__seqToRE`);
   `%:z` requires its colons, the rest is reported as "Missing colon in %:z before '30', got
   ..."; "%:M" reports "':M' is a bad directive".
8. **getargs s#/y*/s*/z#** with an object without buffer interface keep PyObject_GetBuffer's
   TypeError ("a bytes-like object is required, not 'NoneType'").
9. **TextIOWrapper**: "utf-16"/"utf-32" on a non-seekable buffer write no BOM (pandas' bz2/xz
   writers rely on it).
10. **type.__new__ called from C with a pending exception** returns NULL with that exception
    (msgspec test_wrong_classvar: CPython's type_new trips over it at its first namespace
    lookup). msgspec: 0 failures now.
11. **Dict watchers** (capi/dict.rs, types/dict.rs): PyDict_AddWatcher/Watch now work. Watched
    dicts are in a side table (`_ma_watcher_tag` is taken by the interpreter caches); the dict
    primitives (set/insert/setdefault/pop/clear/merge-into-empty = CLONED/popitem/dealloc)
    notify before the change; failing callbacks are unraisable with CPython's message.
    Fast path: one relaxed atomic load while nothing is watched. Not hooked: mutations that
    bypass `types::dict` (interpreter/JIT store fast paths, if any write the table directly).
12. **bz2**: the "BZh" header is written with the first block (compress() returns b"" until
    then); bz2 and xz decompressors consume the stream trailer in the call that returns the
    last output byte (eof/unused_data right away, as libbz2/liblzma).
13. **tokenize**: `tokenize._tokenize` exists; TokenizerIter runs each next() under the
    object lock; without extra tokens ENDMARKER is at (last line, -1) with that line's text.

14. **sys.getrefcount and CPython 3.14+ reference ownership** (sys_full.rs): pandas detects
    chained assignment by comparing `sys.getrefcount(self)` with CPython's counts (2 in
    `__setitem__` of a temporary, 1 in a method of a temporary). CPython loads a local
    argument borrowed (LOAD_FAST_BORROW) and moves a Python call's arguments from the
    caller's stack into the callee frame; our interpreter owns/copies them. getrefcount now
    leaves out (a) the argument's own stack reference when a fast local of the calling frame
    holds the object, and then (b) the parent frame's call operand when that frame is
    executing CALL/CALL_KW with the object on its value stack. Results match CPython for
    named/temporary objects in setitem, methods, plain arguments (also with the JIT on);
    spec test_list.test_list_overwrite_local passes now. This is an emulation in
    getrefcount only (Py_REFCNT from C still sees the real count); the real fix would be
    borrowed stack references + argument stealing in the interpreter/JIT.
15. **csv.Sniffer**: only ASCII characters are delimiter candidates (a lone "�" line is
    "Could not determine delimiter"); when no punctuation qualifies, letters/digits are
    tried, the highest character winning a tie ("aob" splits at "o").
16. `'x' in obj` without __contains__/__iter__: the message names tp_name.
17. **LZMA normal mode** (crates/compress/src/lzma/enc.rs, flagged): presets 4..9 used a
    greedy/lazy parser that compressed no better (sometimes worse) than the fast presets;
    pandas checks `preset=1` output is larger than the default. Mode 2 now plans each
    4096-byte window as a shortest path over positions (literal / short rep / rep0..3 of
    every length / every match length at the nearest distance reaching it, including
    2-byte matches closer than 128), priced from the model's probabilities, each node
    carrying its coder state and recent distances; a match or repeat >= nice_len is taken
    whole and ends the window. A plan always sees a full window + lookahead (or the end of
    input), so streamed and one-shot output stay identical (test_lzma test_write_10).
    Sizes: pandas json 1040 -> 568, csv 508 -> 400, 70 KB Python source 14628 -> 13916
    (preset 6). Speed ~1.3 MB/s on text at preset 6 (was much faster, much worse).
    Dev aid: `cargo run --release -p rpy-compress --example xzsize -- FILE...`.

## Left (root causes known)
* xz presets (3 tests, io/test_compression::test_xz_compression_level_read): our LZMA encoder
  has no price-based optimal parser, so presets 1..9 give the same size on pickled frames.
* test_capi.test_watchers: type/function/code/context watchers still do not fire (28).
