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

## Measurements
(filled in below)

## What is left (measured causes)
(filled in below)
