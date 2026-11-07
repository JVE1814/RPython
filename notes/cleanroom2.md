# cleanroom2 — continuation of the D19 audit (see notes/cleanroom1.md for the method)

## Method
- Same as cleanroom1: a module flagged as recalled is rewritten without opening the old file,
  from the documented API, the spec tests and a grep of crates/runtime + Lib for the names
  other code uses.  Tests run on an installed snapshot (target/cleanroom2/snap) with the old
  file ("before") and the new one ("after"), same binary.
- Error messages that no test asserts were reworded in own words; messages tests assert are
  kept verbatim (listed per file).
- Hot-path modules were timed old-vs-new with a micro benchmark loading each version from a
  file (scratch script; numbers below are ns/op on the shared, noisy machine).

## Rewritten

| file | tests before -> after | names/texts kept because pinned |
|------|-----------------------|---------------------------------|
| functools.py | test_functools 338 ok =; test_inspect 382, test_typing 731, test_genericalias, test_annotationlib, test_reprlib, test_call, test_pydoc, test___all__, test_enum, test_dataclasses = (all pass) | `_CacheInfo` (namedtuple "CacheInfo"), `_c3_mro`, `_compose_mro`, `_find_impl` (tests), `_c3_merge`, `RLock` (test_functools uses `py_functools.RLock`), `_unwrap_partial`, `_unwrap_partialmethod` (inspect), `__partialmethod__` marker on partialmethod's functions (inspect), `cached_property.attrname/.func` (inspect), `singledispatchmethod.dispatcher/.func`, `_PlaceholderType`, `Placeholder`; messages asserted by tests (Placeholder/partial call errors, register()/annotation errors, cached_property errors, "Ambiguous dispatch", "requires at least 1 positional argument", "the first argument must be callable") |
| weakref.py (+ _weakrefset.py: `_IterationGuard` alias dropped) | test_weakref 140 ok = (also with -u cpu); test_weakset 46, test_copy, test_tempfile, test_abc, test_threading_local, test_finalization, test_logging, test_asyncio.test_tasks, test_functools, test___all__, test_pydoc = (all pass) | `KeyedRef` (public, `.key`), `.data` of both weak dicts (WeakValueDictionary's holds KeyedRefs; tests read it), `_remove` without closure (cpython_only test), `keyrefs/valuerefs/itervaluerefs`, finalize `alive/atexit/peek/detach`, "Keyword arguments are not supported: ..." (asserted) |

### functools.py notes
- New structure: total_ordering derives methods from five small factories keyed by a recipe
  table (the generated methods get a class-qualified `__qualname__`/`__module__`, so they pickle
  by reference); cmp_to_key returns a closure over one module-level `_ComparisonKey` class;
  pure-Python LRU uses an OrderedDict (move_to_end/popitem) under an RLock, with keys built by
  `_call_key` (a lone int/str is its own key, otherwise a `_CallKey` holding the argument
  tuple and its precomputed hash); partialmethod's unbound form delegates to a
  `partial(func, Placeholder, *args)`, which also handles Placeholders in partialmethod.
- `functools._singledispatchmethod_get` is now `_DispatchingMethod` (Lib/inspect.py's
  `_DISPATCH_GETTERS` updated).  It has slots plus a `__dict__` holding only `__module__`
  and `__doc__`; other attributes are forwarded to the decorated function (through `__func__`
  for classmethod/staticmethod).  Class-level calls of a plain-function singledispatchmethod
  (`C.generic(instance, arg)`) dispatch on the argument after the instance.
- reduce (Python version) takes `*args, **kwargs`; `function=`/`sequence=` keywords still
  work with a DeprecationWarning.
- Timing (old -> new, ns/op): singledispatchmethod call 895 -> 840, wraps 1299 -> 877,
  singledispatch 1245 -> 1217, total_ordering derived ops 318 -> 329 (noise), cached_property
  231 -> 233, lru (C wrapper) 293 -> 279.
- Behaviour changes: partial.__setstate__ copies the namespace dict instead of sharing it;
  `@register` on a function annotated only for 'return' is rejected; wording of untested
  error messages.

### weakref.py notes
- New design: no iteration guard / deferred removals.  Iterating a weak dict walks a tuple
  snapshot of its entries (tuple(), not list(): list(dict) is ~3x slower on our runtime) and
  skips dead ones; removal callbacks always delete at once (WeakValueDictionary through
  `_remove_dead_weakref`, so a stale callback never drops a newer value).  Removal callbacks are
  default-argument functions made by `_value_remover/_key_remover` (no closure).
- WeakMethod's two weak references share a `_MethodDeathNotice` callback object that holds the
  WeakMethod weakly (no cycle, callback runs once).
- finalize: state lives in `finalize._live` (finalizer -> `_Finalizer` record, slots class);
  the exit hook sorts the atexit-enabled finalizers by creation serial and re-sorts whenever a
  registration happened meanwhile (a finalizer created by a finalizer runs next); the cyclic
  GC is paused during that pass; finalizers triggered after it do nothing.
- `_collections_abc.MutableSet.register(WeakSet)` now lives in weakref.py (it was in the old
  file; _weakrefset is imported too early to do it).
- Timing (old -> new, ns/op, interleaved minima): WeakKeyDictionary set/get/contains equal,
  iteration ~equal (57-84 vs 47-81, noisy), WeakValueDictionary items 155 -> 129,
  finalize create+call 170 -> 159, WeakMethod 89 -> 82.
- Behaviour change: entries whose referent died during an iteration are removed immediately
  instead of when the last iterator finishes (len() is exact either way).

## Text fixes (structure already own; recalled text replaced)
- pickletools.py — audited: own condensed structure (opcode `_TABLE`, `_fixed/_counted`
  helpers, own dis/optimize code), but the read_* doctest examples were CPython's, as were
  optimize()'s docstring and the argparse help strings.  Replaced with own examples/wording.
  Kept (asserted by test_pickletools): every dis()/reader error message, the names listed in
  its test__all__, and the first lines of the opcode docs shown by `--annotate` (PROTO, FRAME,
  SHORT_BINUNICODE, MEMOIZE, TUPLE1, STOP).  The other opcode docs are short paraphrases.
  test_pickletools 202 =, test_pickle 1085 =.

### glob.py (rewritten)
- test_glob 22 =; test_fnmatch 24, test_shutil 227, test_tarfile 781, test_pathlib 1398,
  test_zipfile 388 =; test_idle = (1 pre-existing calltip failure).
- New structure: one `_Expander` object per call (file-system helpers honouring root_dir and
  dir_fd, `expand()` recursing on the directory part, `_literal_child`/`_matching_children`/
  `_descendants` for the last component); iglob filters out the empty result instead of
  special-casing a leading '**'.  translate() builds the regex per component from
  `fnmatch._translate` (output strings are pinned by test_glob and unchanged).
- Kept: `glob iglob escape translate` and `has_magic` (public, widely used); audit events
  "glob.glob" and "glob.glob/2".  Dropped: the private helpers (`_iglob`, `_glob0/1/2`,
  `magic_check`, ...) and the deprecated `glob0`/`glob1` (their deprecation named 3.15 as the
  removal version; nothing in Lib or the tests uses them).
- Timing (same results): '**/*.py' over the stdlib ~equal, '**' with include_hidden 21 -> 8 ms.

### types.py (rewritten)
- test_types 136 =, test_dynamicclassattribute 12 =, test_coroutines 100 =,
  test_asyncio.test_pep492 11 =, test_asyncio.test_tasks, test_inspect 382, test_enum 1083,
  test_typing 731, test___all__, test_pydoc, test_functools =; test_descr = (1 pre-existing
  failure + 1 error, same before/after).
- Kept (pinned): `_GeneratorWrapper` (tests), DynamicClassAttribute attributes
  `fget/fset/fdel/overwrite_doc/__isabstractmethod__`, "Expected an instance of type",
  "types.coroutine() expects a callable".  Dropped: `_calculate_meta` (now
  `_winning_metaclass`), `_cell_factory`, `_CO_*` constants (now `_FLAG_*`).
- The no-`_types` fallback (exercised by test_types via import_fresh_module) derives every
  type from a sample object; `__all__` is `_types.__all__` (or the fallback list) plus the
  helpers.  `_GeneratorWrapper` forwards gi_*/cr_* through properties made by one helper.

### string/__init__.py (rewritten)
- test_string 55 =, test_logging 289 =, test_configparser 383 =, test_pydoc, test___all__ =.
- Template: the base class compiles its pattern on first use through a small descriptor
  (`import string` still does not import re); every subclass compiles its own pattern when
  created (an explicit pattern string, a precompiled re.Pattern used as is, or the default
  built from its delimiter/idpattern/braceidpattern).  Keyword arguments shadow the mapping
  via `_KeywordsFirst`.  Kept (asserted): "Invalid placeholder in string: line L, col C",
  `flags = None` meaning re.IGNORECASE (as before).  Behaviour change: the line/column of an
  invalid placeholder right after a line break is now reported on the new line (col 1).
- Formatter: kept `_vformat(format_string, args, kwargs, used_args, recursion_depth,
  auto_arg_index=0)` (test_string calls it); numbering logic moved to `_number_field`; the
  manual-numbering state is None instead of False; str.format's own error texts kept.
- string/templatelib.py reviewed: tiny, own (`_template_unpickle` is used by the runtime).

### queue.py (rewritten)
- test_queue 165 = (C and Python variants), test_logging 289 =, test___all__ =,
  test_concurrent_futures.test_thread_pool = (1 pre-existing test_free_reference failure).
- Kept: the documented attributes (`mutex not_empty not_full all_tasks_done
  unfinished_tasks maxsize queue is_shutdown`), the storage hooks `_init _qsize _put _get`
  (subclass API, the tests override them), `_PySimpleQueue` (tests), message "task_done()
  called too many times" and the negative-timeout ValueError.  put()/get() have an inline fast
  path and share one waiting helper (`_wait_until`) for the blocking cases; shutdown
  semantics as documented.  Timing (put+get, uncontended, ns): ~equal (noise > difference).
- http/cookies.py — audited: body is own code (own `_has_control_character`, `_escape_char`,
  regex-based unquoting), but kept CPython's helper names and `_getdate`'s signature.
  Renamed `_LegalChars/_UnescapedChars/_quote/_unquote/_getdate` to own names and rewrote the
  date helper.  Kept `Morsel._reserved` (asserted by test_http_cookies).
  test_http_cookies 35 =, test_http_cookiejar 81 =.
