# perf5 workstream — exceptions, attribute/call paths in tier 1, GC roots, pathlib

Branch: the perf5 worktree (based on `core` 20d1678 = master + perf4). Everything written
from scratch. Method as perf3/perf4: callgrind instructions are the primary metric
(`tools/bench/perloop.sh`, `tools/bench/micro/percall.sh`); wall clock through
`tools/bench/run_bench.sh` pinned to core 3, back to back with the baselines, is
indicative only (D22; load 5-7 from other agents, single runs vary by 10-15%).

New tooling:
* `tools/bench/micro/pathops.py` (percall kinds `stat`, `newpath`, `newstr`, `pstat`,
  `iterdir`, `glob`, `attrerr`, `attrerr_call`, `newinit`, `withseg`, `superinit`,
  `fspath`): per-operation costs of the pathlib benchmark's building blocks.
* `tools/jit_tests/t_exc_handlers.py` (raise/re-raise and handlers in compiled frames:
  tracebacks, context/cause, sys.exception(), finally/with, loops, generators),
  `t_props.py` (property getters through the call routine, bound method objects),
  `t_attrs2.py` (attribute-adding stores, LoadMethod of module functions, getset
  getters, shadowable class attributes), `t_classattrs.py` (class attributes under
  ABCMeta/custom metatypes incl. classmethod/staticmethod and later modification of
  class and metatype, namedtuple fields, type(x)).

## Decision proposal (for DECISIONS.md, coordinator)
**`.cargo/config.toml` rustflags gain `-Z plt=yes -Z relax-elf-relocations=yes`.** Rust
emits cross-codegen-unit calls as `call *GOT(%rip)` and data addresses as GOT loads by
default; with the version script of perf4 (only the C API exported) the linker can
relax both into direct `call`/`lea` once the flags are given (checked in the disassembly
of `alloc_object`: `mov GOT(%rip)` became `lea sym(%rip)`). Verified: the C API still
loads (test_capi.test_misc/test_unicode/test_dict/test_long/test_object and test_ctypes:
identical results to the starting point, 774 run / 42 failures both), the spec's test
extensions build and import, and the numpy 2.5.4 cp315t wheel imports and computes.
(RUSTC_BOOTSTRAP=1 was already set for D18.) Instruction counts are unchanged by it;
the gain is wall time (perf4 measured go -4%, float -3%).

## Changes

### Exceptions in compiled code (item 1)
* `RaiseVarargs`/`Reraise` are compiled in tier 1 (`helpers::h_raise` → shared
  `eval::raise_op`, also used by the interpreter): they used to deoptimise, so every
  `raise` in a hot function sent the rest of the activation to the interpreter.
* `eval::unwind_compiled` replaces perf4's `unwind_compiled_error`: an exception raised
  (or re-raised, with `f.unwind_pc`) in a compiled call frame is unwound in Rust exactly
  as `eval_inner` does (context chaining, traceback entry, propagation mark, stack cut
  to the handler depth, lasti/exception pushed); with a handler in the frame
  `jit::finish_in_interp` re-enters the compiled code at the handler (`jit::reenter`,
  tier-2 entry when there is one) and loops on further exceptions; without one it
  propagates (perf4's path). Only tracing still uses the interpreter.
  `RPYTHON_JIT1_RAISE=0` makes raise/reraise deoptimise again.
* calls.py `exc` 4779 → 4042 instructions (CPython 3686).

### Calls (items 2, 3)
* Property getters (LoadAttr/LoadMethod hit on an exact `property` whose getter is a
  Python function with a fast entry): tier 1 lays the stack out as [fget, obj] (spare
  slot above the stack) and calls jit6's machine-code call routine. calls.py `prop`
  626 → 401 (CPython 441).
* Bound method objects at `Call` sites with a free self slot are unpacked in machine
  code (`h_unpack_method`) and take the call routine. `bound` 706 → 447 (CPython 502).
* `type(x)` at a `Call` site with one argument is inline (CPython's CALL_TYPE_1).
* Keyword binding fast path (`interp::bind_kw_fast`): keywords naming parameters by
  identity, defaults/kwdefaults filling the rest, no *args/**kwargs; anything else
  (and every error) goes through the general binder from scratch. `kw` 836 → 743
  (CPython 701).

### Attributes in tier 1
* Attribute-adding stores (`self.x = ...` in `__init__`): the StoreAttr INSTANCE_DICT
  case knows the one shape transition adding the name at the hinted index
  (`shape::transitions_adding`, as tier 2 already did): shape == parent and room left →
  store and move to the child shape, single-threaded. The runtime path got the same
  shortcut (`shape::add_hinted`) instead of `set_ix`'s key scan.
* LoadMethod of plain attributes (module functions `os.fspath(...)`, class attributes
  without binding, instance attributes, slots, properties) uses the LoadAttr inline
  cases and NULLs the self slot (it went to `h_load_method` every time).
* Getset descriptors (frame.f_back, f_code, code.co_filename, `__dict__` of heap
  types, ...): the C getter is called directly after the type guard.
* CLASS_ATTR_SHADOWABLE (class attribute the instance could shadow) inline with the
  shadow check METHOD_SHADOWABLE had (`lacks_name`, shared now).
* namedtuple fields (`_tuplegetter`): the tuple item directly (tuple-subclass flag and
  size checked).
* New cache kinds for class objects whose metatype derives from `type` without its own
  lookup (ABCMeta, enum's, typing's...): TYPE_ATTR_META (plain value), TYPE_CLASSMETHOD
  (LoadMethod pushes the function with the class as self — no bound method per call;
  LoadAttr binds), TYPE_STATICMETHOD; guarded by class identity, class tag and metatype
  tag (a = class tag | metatype tag << 32). Interpreter, helper paths and tier 1; tier 2
  ignores them (template). Fixes e.g. `cls._from_coprime_ints(...)` in fractions (mdp),
  which was never cached.

### GC (gc_traversal)
* `find_unreachable` first marks everything reachable from the locals (and function,
  globals, builtins, locals mapping) of every frame running on a registered thread,
  plus the current interpreter's `sys.modules` and `sys` (raw pointers: refcounts must
  not move while owner words hold collection state); trial deletion then only
  traverses the objects not reached. Each edge of the live heap is visited once
  instead of twice. Only the locals part of frame slots is read (compiled code does not
  keep `stack_top` current; local slots always hold owned references or NULL).
* The visit of an examined object reads only the owner word (the GRAPH_TAG alone
  identifies the examined objects; the tracked-bit load and the null check of the
  inline list/tuple paths are gone); the mark stack is reserved once.
* gc_traversal wall: 1.50x → ~0.9x of the best CPython (free-threaded 3.15 is the best
  there); gc_collect 0.50x → 0.42x.

### Library / runtime
* pathlib (our own Lib): `_from_str` builds child/glob paths directly when the class
  keeps the default construction (`with_segments`, `__new__`, `__init__` identical to
  PurePath's/Path's; otherwise `with_segments` as before); glob results keep their
  (already normal) string (`_from_parsed_string`: no lazy parse with two
  AttributeErrors per `str()`); `PurePath.__init__` fast path for exact str; the
  globber takes `(name, entry)` pairs plus an `is_dir(entry, follow)` function instead
  of a closure per directory entry (abstract paths keep the old protocol).
  pathlib perloop 145.3M → 87.9M (CPython 87.2M).
* os.stat: stat_result built from an array (no Vec), structseq::new_instance takes any
  exact-size iterator; path encoding reserves the terminator (no realloc in
  CString::new).
* Tuple equality (`plain_eq_depth`): identical items and compact ints compared inline.
* async generators: per-thread free lists (32 each) for wrapped values and asend
  awaitables (`alloc::reinit_header`, `tlab::fast_free_ok`; drained at thread exit).
  async_generators 3839M → 3233M per loop (CPython 3871M).

* (coordinator note) `list(d)`/`tuple(d)`/`list.extend(d)` for exact dicts, exact dict
  views and exact sets take one snapshot under the container's lock
  (`abstract_::builtin_snapshot`) instead of iterating; dict iterators take only the
  references they yield (keys: no value incref/decref). `list(d)` for a 100-key dict
  22185 → 5585 instructions, `tuple(d)` 21772 → 6554, `list(d.values())` 22553 → 6653,
  `list(set)` 22785 → 12815.

## Measurements
"base" = `core` 20d1678 (perf4 merged) built in this worktree; "perf5" = branch head.

### Instructions per operation (`percall.sh`, includes the loop)
| kind | base | perf5 | CPython default (perf4 table) |
|---|---|---|---|
| calls.py kw `f(i, b=2)` | 836 | 743 | 701 |
| calls.py bound `m(i)` | 706 | 447 | 502 |
| calls.py prop `o.p` | 626 | 401 | 441 |
| calls.py exc (raise + catch one level down) | 4779 | 4042 | 3686 |
| calls.py getattr_call `getattr(o,'m')(i)` | 1371 | 1116 | 1094 |
| calls.py init / super / star / keyerror | 755 / 2166 / 617 / 2280 | 747 / 2175 / 628 / 2270 | 1253 / 1911 / 856 / 1841 |
| pathops.py iterdir (per entry) | 14319 | 8737 | 8893 |
| pathops.py glob `*.py` (per scanned entry) | 17467 | 11331 | 10909 |
| pathops.py stat / pstat | 5316 / 6862 | 5033 / 6562 | 4706 / 6190 |
| pathops.py newstr `str(Path(text))` | 33340 | 30404 | 25568 |

(CPython columns for pathops measured here through percall.sh; they run CPython's own
pathlib.)

### Steady state per benchmark loop (callgrind, millions, `perloop.sh`)
| bench (windows) | base | perf5 | CPython default (perf4/jit3 tables) |
|---|---|---|---|
| deepcopy (50/300) | 3.46 | 3.13 | 2.83 |
| logging (500/1500) | 1.44 | 1.24 | 1.05 (measured here) |
| typing_runtime_protocols (300/1300) | 2.08 | 1.94 | 1.73 |
| pathlib (2/6) | 145.28 | 87.92 | 87.2 |
| async_generators (1/2) | 3813.95 | 3343.52 (3233 with the dict change) | 3871 |
| deltablue | 40.75 | 36.97 | 29.1 |
| float | 575.55 | 563.25 | 778 |
| chaos | 353.41 | 347.94 | 611 |
| nqueens | 847.64 | 841.78 | 842 |
| json_dumps | 83.03 | 81.32 | 102.1 |
| richards / go / raytrace / hexiom | 264.4 / 1031.0 / 2337.3 / 57.3 | 264.6 / 1030.9 / 2330.6 / 57.3 | 461 / 1138 / 2757 / 58.1 |
| generators / coroutines | 275.4 / 280.4 | 274.6 / 278.5 | 337 / 294 |
| mdp (1/2) | 13186.6 | 12603.9 (before the tuple-equality change) | - |

gc_traversal: `find_unreachable` 16M → 6M instructions per collection of the
benchmark's heap (1000 lists, 500k references).

### Wall clock
s/loop through `run_bench.sh`, the four binaries back to back per benchmark, pinned to
core 3, one run each, load 6-7 from other agents: indicative only (D22). ratio = x /
best CPython. (A second base/perf5 A/B of asyncio_tcp gave 0.86/0.67 and 0.88/0.84 s,
meteor_contest has identical instruction counts in both builds (969M/loop): their
"worse" entries below are noise.)

| bench | base | perf5 | CPython default | CPython ft | base ratio | perf5 ratio |
|---|---|---|---|---|---|---|
| pathlib | 0.01758 | 0.01085 | 0.009387 | 0.01030 | 1.87 | 1.16 |
| asyncio_tcp | 0.7472 | 0.8243 | 0.6367 | 0.6452 | 1.17 | 1.29 (noise, see above) |
| concurrent_imap | 0.02842 | 0.03066 | 0.02844 | 0.03247 | 1.00 | 1.08 |
| deepcopy | 0.0002358 | 0.0002107 | 0.0001549 | 0.0001727 | 1.52 | 1.36 |
| mdp | 1.418 | 1.324 | 1.063 | 1.166 | 1.33 | 1.24 |
| gc_traversal | 0.001597 | 0.000788 | 0.002695 | 0.001125 | 1.42 | 0.70 |
| xml_etree | 0.1554 | 0.1560 | 0.1005 | 0.1079 | 1.55 | 1.55 |
| typing_runtime_protocols | 0.0001537 | 0.0001403 | 0.0001119 | 0.0001326 | 1.37 | 1.25 |
| logging | 5.694e-06 | 5.539e-06 | 4.261e-06 | 5.113e-06 | 1.34 | 1.30 |
| bpe_tokeniser | 4.110 | 4.208 | 3.239 | 3.393 | 1.27 | 1.30 |
| base64 | 0.0002603 | 0.0002398 | 0.0002028 | 0.0002154 | 1.28 | 1.18 |
| python_startup | 0.01255 | 0.01224 | 0.009469 | 0.01127 | 1.33 | 1.29 |
| pprint | 0.6384 | 0.5891 | 0.5764 | 0.6040 | 1.11 | 1.02 |
| async_generators | 0.2893 | 0.2721 | 0.2321 | 0.2773 | 1.25 | 1.17 |
| gc_collect | 0.0005855 | 0.0004573 | 0.0008981 | 0.0008881 | 0.66 | 0.51 |
| go | 0.08126 | 0.07662 | 0.07904 | 0.09068 | 1.03 | 0.97 |
| richards | 0.01908 | 0.01895 | 0.03093 | 0.04062 | 0.62 | 0.61 |
| nqueens | 0.07318 | 0.06435 | 0.05623 | 0.06505 | 1.30 | 1.14 |
| generators | 0.02078 | 0.02163 | 0.02078 | 0.02353 | 1.00 | 1.04 |
| comprehensions | 1.404e-05 | 1.322e-05 | 1.038e-05 | 1.136e-05 | 1.35 | 1.27 |
| json_dumps | 0.005059 | 0.004802 | 0.006369 | 0.006929 | 0.79 | 0.75 |
| meteor_contest | 0.08913 | 0.09208 | 0.07876 | 0.09499 | 1.13 | 1.17 (noise) |

### Start-up
`-c pass`: 56.9M → 55.8M instructions (env lookup per translated code object, GC
collections at exit cheaper with the roots).

### Memory
startup_mem.sh peak_rss_kb: 35292 KB (base 34928 per perf4; CPython default 32372,
measured here through startup_mem.sh). Unchanged in substance (see "What is left").

## Correctness
tools/jit_tests/run.sh (with the new t_exc_handlers, t_props, t_attrs2, t_classattrs)
and tools/stress/run.sh pass on the final build. Spec modules run with tools/run_tests.sh
on installed-layout snapshots of the starting point and of the head: identical
results (run/failures/errors per module) for test_call test_descr test_class
test_exceptions test_traceback test_sys_settrace test_monitoring test_generators
test_coroutines test_logging test_pathlib test_xml_etree test_copy test_typing test_gc
test_threading test_free_threading test_pickle test_asyncio test_dataclasses
test_inspect test_frame test_property test_extcall test_abc test_collections
test_fractions test_enum test_functools test_weakref test_glob test_os test_asyncgen
test_tuple test_dict test_dictviews test_set test_list test_ordered_dict test_userdict
(test_free_threading.test_mmap's `test_mmap_export_as_memoryview` is flaky on both:
BufferError in ~1 of 4 runs). C API: test_ctypes + test_capi.{test_misc, test_unicode,
test_dict, test_long, test_object} identical (774 run, 42 failures); numpy 2.5.4 wheel
imports and computes.

## What is left (measured causes)
* xml_etree (~1.5x, the benchmark's first value is `etree.parse`): the expat engine
  (pyexpat/engine.rs) decodes the whole input into a `Vec<char>` plus a per-char width
  vector, keeps one `Event` with an owned `String` per piece in a vector for the whole
  feed, and `_elementtree` turns each into Python objects afterwards: ~30M instructions
  per `fromstring` of the benchmark document (5150 elements), half of it malloc/free
  and Vec growth. A byte-based scanner emitting Python objects directly is the fix
  (owner: markup).
* mdp (~1.45x): tuple-keyed dict lookups (dict::find + tuple equality ≈ 15% of
  instructions; each lookup with a fresh key compares ~11 nested items), generator
  expressions created per call (function + generator + frame, ~700 instructions) and
  resumed from Rust (`gen_send_fast`, ~100 per item), Fraction arithmetic through
  `object.__new__` and `math.gcd`.
* Calls that do not go through the machine-code call routine still cost ~250-300
  instructions: calls from Rust (`vectorcall_raw` → `call_function` → `call_bound` →
  `enter_bound` → `run_call_frame`), class instantiation (`type_vectorcall` does three
  MRO lookups, then two Python calls for `__new__`/`__init__`; `newinit` micro 4914 vs
  3454), CallKw, CallFunctionEx. typing_runtime_protocols (1.94M vs 1.73M per loop) and
  deepcopy (3.13M vs 2.83M) are dominated by these plus per-call allocation.
* `raise` + catch one frame down still 4042 vs 3686: AttributeError's own `__init__`
  goes through type_call (not `make_exception`), and every traceback entry allocates a
  frame object + traceback object (~600 instructions with the frees).
* Start-up 55.8M vs 40.5M instructions: CPython runs the startup modules frozen;
  here each import goes through the path finder, reads the pyc, unmarshals (8.4M),
  translates the public bytecode on first execution (10.5M for 233 code objects) and
  runs `importlib._bootstrap(_external)` bodies (7M). Caching translated code or
  freezing the startup modules is the next step; three full collections at exit cost
  ~4M (with the frame/sys.modules roots most of the live heap is now visited once).
* Peak memory (35.3 vs 32.4 MB on startup_mem.sh): the free-threaded object header is
  part of the C ABI (an int takes 48 bytes vs 32, a short str 64 vs 48; ~2.5 MB on this
  workload's 100k ints + strs); file-backed RSS of the executable after start-up is
  7.4 MB (3.6 MB text resident of 9.3 MB — the startup-ordered section alone is 2.7 MB —
  and 1.4 MB of rodata/eh_frame through fault-around). MADV_RANDOM on the text would
  trade RSS for page faults at start-up; not done.
* list(set) is still ~2x list(dict) per item (`set_items` walks the whole table).
