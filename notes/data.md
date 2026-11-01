# Workstream "data": serialisation and date/time modules

All Python files below were written clean-room (rule 9 / D19). Error messages
and other strings that the tests check match CPython; all other text,
including docstrings, is the project's own.

## Native (Rust) modules — `crates/runtime/src/modules/`

| module | file | notes |
|---|---|---|
| `array` | `array.rs` | All typecodes (`b B u w h H i I l L q Q e f d Zf Zd`), buffer protocol with an export count, slicing, pickling through `_array_reconstructor` (machine format codes), iterator pickling, registered as a `MutableSequence`. |
| `_datetime` | `_datetime.rs` | The object layouts are exactly `PyDateTime_Delta/Date/Time/DateTime/TZInfo` from `Include/datetime.h`. The module exports the `datetime.datetime_CAPI` capsule (`PyDateTime_CAPI` in header order). The methods come from `Lib/_pydatetime.py`: its source is embedded with `include_str!`, run at first import, and has its class names rebound to the native types. The pure code only touches instance fields through `_year`/`_month`/... getsets, and `tp_new` validates through the `_*_new_args` helpers. `total_seconds` and `isocalendar` are implemented natively. |
| `_pickle` | `_pickle.rs` | Pickler/Unpickler/PickleBuffer, plus the memo proxies. |
| `_json` | `_json.rs` | `scanstring`, `encode_basestring(_ascii)`, `make_scanner`, `make_encoder`. |
| `_csv` | `_csv.rs` | `Dialect`, the reader state machine, the writer, the dialect registry and `field_size_limit`. |

## Python modules — `Lib/`

`pickle.py`, `pickletools.py`, `_compat_pickle.py`, `json/*`, `_pydatetime.py`, `datetime.py`,
`calendar.py`, `configparser.py`, `csv.py`, `shelve.py`, `fileinput.py`, `plistlib.py`,
`zoneinfo/{__init__,_common,_tzpath,_zoneinfo}.py`.

## Test status (on this branch)

* Fully passing: test_array, test_json, test_pickletools, test_csv, test_calendar, test_configparser, test_shelve and test_fileinput.
* test_datetime: 1160 run, 4 failures:
  * 3 × `test_static_datetime_types_outlive_collected_module`. `import.rs` keeps the first builtin module object alive forever (`note_builtin_module`), so `_datetime` is never collected and the expected ImportError never fires.
  * `test_concurrent_initialization_subinterpreter` needs `concurrent.futures`.
  * With `_testcapi` built by `tools/build_test_exts.sh`, all `CapiTest_Fast` tests pass.
* test_pickle: the remaining failures are in the runtime:
  * the interpreter's `**` error message;
  * surrogate module/attribute names;
  * cpython_only sizeof/gc tests.
* test_plistlib: every binary-format test passes, and the XML writer is byte-exact against Apple's data. XML reading needs `xml.parsers.expat` (markup workstream).
* test_zoneinfo needs `lzma` and `importlib.metadata` just to import. I ran a local copy of the test (outside the repo) against `/usr/share/zoneinfo`:
  * TZStr, WeirdZone and TzPath tests pass, and so do the cache, pickle and subclass tests.
  * The only `dst()` mismatches come from this system's tzdata, which is built with positive DST and so disagrees for zones that use negative DST.
  * `CZoneInfoCacheTest.test_deleted_weak_cache` expects the C module's cache layout. There is no `_zoneinfo` accelerator yet.

## Design decisions

* `_datetime` is a hybrid, so that there is one source of truth for the behaviour. Changing `Lib/_pydatetime.py` requires rebuilding the runtime, because the source is embedded.
* `_pydatetime.datetime.astimezone()` reads naive values through the fold-aware local timestamp, which makes gap and fold handling correct.
* `zoneinfo`:
  * Daylight-saving rules from POSIX TZ strings are evaluated over a window of three years, which handles transitions that cross year boundaries.
  * When TZ strings fail to parse, the error message contains both the decoded text and the raw footer bytes. This satisfies the pure and the C variants of the tests.
* `_tzpath`: when neither `PYTHONTZPATH` nor `sysconfig` gives a TZPATH, it uses the conventional system directories.

## Runtime fixes made outside my own files

* `list_methods.rs` / `list.rs`: `getitem_m` of a list subclass no longer recurses.
* `funcobject.rs`: method `__reduce__` mangles private names.
* `typeobject.rs` and `cfunction.rs`: static-method `self` handling.
* `descr.rs`: `classmethod_descriptor` is not picklable.

## Missing / handed over

* `dbm` now belongs to the "tty" workstream; my `dbm/__init__.py` and `dbm/dumb.py` still need its clean-room rewrite.
* A real `_zoneinfo` accelerator does not exist yet.
* `pprint`, `string`, `_strptime`, `locale`, `copy` and `copyreg` are owned by other workstreams.
