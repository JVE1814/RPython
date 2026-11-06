# Workstream "numpy": binary cp315t wheels on our interpreter

## Fixed
1. **Code objects created from C** (`PyUnstable_Code_New`, `PyUnstable_Code_NewWithPosOnlyArgs`,
   `PyCode_NewEmpty`; capi/funcs.rs `new_code_from_c`). Cython 3.3 makes one for every
   function (`__code__` of cyfunctions) and one per traceback line. They hold the caller's
   argument counts, flags, varnames/cellvars/freevars (as localsplus with CPython's kind
   bits), co_consts/co_names, filename, name/qualname and first line. The CPython-format
   bytecode, line table and exception table are accepted and ignored. The executable body is
   a shared stub (compiled once from `assert 0`), so running one raises AssertionError, as
   test_code.test_newempty expects. Every instruction maps to the first line. The old
   `PyUnstable_Code_New` stub also had the wrong parameter list (posonly is not a parameter
   there); that is fixed.
   Note: Cython writes `code->_co_firsttraceable` directly. That field lands in
   `PyCodeObject._reserved[5]`, so the reserved words must stay.
2. **Segfault in frame.f_locals on Cython traceback frames.** Cython's `__Pyx_AddTraceback`
   calls PyFrame_New and then stores `frame->f_lineno = line` at CPython's internal offset 56.
   In our frame object that offset was `locals_snapshot`, a pointer that was later decref'd
   or read. `PyFrameObject` now keeps CPython's offsets for f_back, f_frame, f_trace, f_lineno
   and f_trace_lines/opcodes (checked by a const assertion). The `f_lineno` getter returns the
   stored value when it is non-zero. PyFrame_New now validates its arguments and, like
   CPython, uses globals as the locals mapping for non-optimized code (`f_locals` of a Cython
   frame is the module dict).
3. **Crash calling a Python class from Cython** (found as soon as numpy.random imported):
   `PyType_Type` had `Py_TPFLAGS_HAVE_VECTORCALL` but `tp_vectorcall_offset == 0`. The
   header-inline `PyVectorcall_Function` (used by Cython's `__Pyx_PyObject_FastCallDict`)
   then read `ob_tid` as a function pointer and jumped to it. The offset is now
   `offsetof(PyTypeObject, tp_vectorcall)`. A class's own `tp_vectorcall` is NULL unless it
   sets one, which means "use tp_call". `type_vectorcall` also handled keyword arguments
   wrongly (it returned NULL); it now builds a tuple and dict.
4. `slot_tp_call` and builtins used as `__call__` (typing.NewType): fixed here, and master fixed it
   the same way (`call_bound_kw`). The merge kept master's version.

Verified with the release build and an unmodified numpy-2.5.4 cp315t wheel installed by pip in a
venv: `import numpy.random` works and `default_rng(1).random(3)` gives the same values as
CPython. A ValueError raised inside Cython code has a traceback whose last frame is named
`numpy.random._bounded_integers._rand_int64`, with file `numpy/random/_bounded_integers.pyx`.
That frame's `f_lineno` equals `tb_lineno` (1334), and `f_locals` is a dict.
`typing.NewType('U', int)(3)` works.

## Tooling
`tools/wheels/run_numpy_tests.sh` (see its header) does the following:
- makes a snapshot with tools/snapshot.sh;
- creates a venv (ensurepip), then pip-installs the packages from PyPI, or from a wheel
  directory with `-w`;
- runs `pytest --pyargs numpy -v --continue-on-collection-errors` under `ulimit -v`;
- writes `ours/{pytest.log,junit.xml,failed.txt,summary.txt}`.
With `-B` it does the same on /opt/baseline/py-ft and writes `only_ours.txt`, the failures that
do not happen on CPython.
- The memory limit (default 8 GB) is needed: numpy tests such as
  `test_strings.py::test_large_string_addition_error` allocate more than 9 GB. Without the
  limit, the kernel's cgroup OOM killer ends the whole run, and this happens on the baseline too.
- The f2py tests (31 modules) fail at collection on both interpreters: they need `meson` and
  a Fortran compiler.
- (Old: master lacked `xmlrpc`; it is merged now.)
- The script now sets `NPY_AVAILABLE_MEM=4GB` (numpy's `requires_memory` skips no longer depend
  on what other processes use) and runs numpy's `test_multithreading.py` in a separate process
  without the memory limit: under `ulimit -v` one of its 250 barrier threads can fail to start
  late in a run, and the others then wait forever. This hung the baseline twice.
- pip installs from PyPI over the network on master (ssl/http/urllib are merged); pip also
  builds sdists with setuptools (PyYAML 6.0.3 was built from source).

# Workstream "numpy2" (second round)

Branch: `worktree-agent-a8c1a16ece096a4ad`. First workstream branch: `worktree-agent-a1d1b4c37f27f2c53`.

## Results (ours vs baseline /opt/baseline/py-ft, same wheels and test tools)

Wheels: numpy 2.5.4, pandas 3.1.0, bitarray 3.12.3, regex 2026.9.29, markupsafe 3.0.4,
msgspec 0.22.0, msgpack 1.2.4 (all cp315t from PyPI); pytest 9.2.0, hypothesis 6.99.9.

| package | tests | ours | baseline | only on ours |
|---|---|---|---|---|
| numpy (full `pytest --pyargs numpy`) | 51,311 | 50,575 passed, 3 failed, 41 errors | 50,574 passed, 5 failed, 41 errors | **0** |
| numpy test_multithreading (separate) | 34 | 34 passed | 34 passed | 0 |
| bitarray (`--pyargs bitarray`) | 742 | 740 passed | 740 passed | 0 (test_free_threading's mutable-exporter test is flaky, see gaps) |
| regex (`--pyargs regex`) | 102 | 101 passed, 1 failed | same | 0 (test_main fails on both: argv) |
| markupsafe (sdist tests/) | 80 | 79 passed | 79 passed | 0 |
| msgpack (sdist test/) | 161 | 160 passed | 160 passed | 0 |
| msgspec (sdist tests/unit) | 6,602 | 6,541 passed, 1 failed | 6,542 passed | 1 (test_wrong_classvar, see gaps) |
| pandas | see below | | | |

The 41 numpy errors (both) are f2py tests (need meson + a Fortran compiler), test_mem_policy
and test_cstruct (need a C build toolchain at test time). The 3 numpy failures (both) are
tests/test_configtool.py (numpy-config tool tests). Baseline-only:
test_regression.py::test_to_ctypes and lib/tests/test_io.py::TestSavezLoad::test_big_arrays.
Ours skips 2 tests the baseline runs: test_subinterpreter_import_fails_cleanly
(`concurrent.interpreters` missing) and test_multithreaded_array_printing (skipped only inside
the long run; passes alone). Timing: ours 15m46s with the tier-2 JIT on (master as of
2026-11-06), baseline 10m12s.

Re-run after merging master e57fa85 (perf2 + D24 wordcode, 2026-11-06 late): ours 7m30s
(faster than the baseline's 10m12s), 50,567 passed; the only new failures were 8 warnings
tests (numpy.testing assert_warns / suppress_warnings, test_assert_warns_deprecated): a
master change made `catch_warnings(record=True)` recording win over *any* replaced
`showwarning`, so numpy's suppress_warnings (which installs its showwarning inside an outer
recording context) never saw its warnings. Fixed in Lib/_py_warnings.py (recording wins only
over a showwarning installed before the recording started); those 8 tests pass again and
test_warnings is unchanged (3 failures before and after, unrelated) -> numpy only-ours = 0.

test_capi (spec): before this round the package did not load at all (load errors) and, once it
loaded, 13 of its 43 modules crashed the interpreter. Now `python -m test test_capi` runs to the
end: 1,531 tests run (baseline 1,562), with failures left in test_modsupport-like areas listed
below. Per-module status: tools/run_tests.sh over test_capi.* (scratch results in
/tmp/rpy-wheels/capi-cur5).

## Fixed in this round (all in the C API or the runtime, none in the packages)
numpy / Cython:
- `types.CodeType(...)` constructor (code_tp_new -> new_code_from_c; metadata-only body like
  the other C-created code objects; nlocals check).
- C-created code objects decode a CPython-format location table (Cython passes one per
  function): `co_lines()`/`co_positions()` report the source lines instead of co_firstlineno.
- `PyUnstable_Object_IsUniqueReferencedTemporary`: refcount 1 *and* the object is an operand
  that BINARY_OP (interpreter or JIT helper) took off the value stack (thread-local record in
  interp/eval.rs `binary_op_on_stack`). Plain refcount==1 let numpy reuse a Cython temporary in
  place. See the JIT note below.
- `PyUnstable_Object_IsUniquelyReferenced` ignores value-stack copies of the calling frame's
  own locals (CPython's LOAD_FAST_BORROW does not count them): `x.resize(...)` on a local works.
- dict: popitem left DUMMY index slots uncounted, so a table could run out of EMPTY slots and
  lookups of a missing key looped forever (hung numpy's suite) -> `fill` counter.
- Static extension types with `ob_type == NULL` get their base's metatype (numpy DTypes).
- Extension static types without their own number/sequence/mapping/async/buffer tables share
  the base's tables (np.str_ * 2 used numpy's generic multiply instead of str's sq_repeat).
- str.encode: ASCII/Latin-1 text is copied straight into the bytes object, MemoryError instead
  of an abort for huge strings; `PyBytesWriter` allocations are fallible.
- Shape (inline attribute) materialisation published the dict after retiring the shape: a
  racing reader saw neither and fell back to the class attribute ("Thread.__init__() not
  called" in numpy's threaded tests). Now the dict is published first.
- `__buffer__(PyBUF_READ)` reaches the type's getbuffer (numpy accepts it); the READ/WRITE check
  moved to PyBuffer_FillInfo where CPython has it.
- PyType_FromSpec keeps a `__module__` member/getset of the type's instances (Cython functions
  pickle by their own module: numpy.random pickling); `type.__module__` then uses the dotted name.
- typing aliases have `__name__`/`__qualname__` (pytest ids).
bitarray / msgspec / pandas:
- Critical section on the same object twice (`Py_BEGIN_CRITICAL_SECTION2(a, a)`) was not popped
  by its End -> corrupted chain -> crash later (`a[::-1] = a`).
- A class defining only `__hash__` lost its base's comparisons (frozenbitarray ==).
- PyDict_* read functions accept frozendict; writes raise "frozendict object does not support
  item assignment/deletion"; PyDict_GetItem reports errors as unraisable; PyDict_Pop on an
  empty dict does not hash. (test_capi.test_dict now passes.)
- `__slots__` members live in the type object's variable part after the metatype's fields, as
  msgspec reads them; keyword-only parameter repeating a positional name in
  PyArg_ParseTupleAndKeywords; heap subclasses of a vectorcall metaclass inherit
  tp_vectorcall_offset (PyVectorcall_Call no longer needs the flag); managed dict/weakref types
  report CPython's sentinel offsets (tp_dictoffset -1, tp_weaklistoffset -16) in C and Python;
  a slotted class gets __dict__/__weakref__ from a secondary base; `PyObject_GenericGetAttr` is
  object's own tp_getattro (pointer comparisons in extensions work); builtin exceptions have
  their own `__new__`.
- timedelta's private `_days/_seconds/_microseconds` getsets (used by the embedded pure-Python
  methods) yield to a subclass's instance dict: pandas' Timedelta assigns `self._seconds`.
test_capi:
- real `instancemethod` type; `_testcapi/bytes.c` builds against tools/capi_shim/pycore_bytesobject.h
  (+ `_PyBytesWriter_CreateByteArray`); pending calls run from the eval breaker
  (`EVAL_PENDING`, main-only queue of 32 and any-thread queue of 300);
  `_testinternalcapi.pending_threadfunc`, `_Py_MAX_UNICODE`; PyABIInfo_Check rules.
- crash fixes: NULL C-string arguments (cstr_obj -> SystemError), globals must be a dict,
  NULL list/tuple items repr as `<NULL>`, size limits in PyTuple_New/FromArray,
  PyUnicode_New/FromStringAndSize (str allocation size overflow could wrap!),
  PyBytes_DecodeEscape, Py_tp_bases given as one type or None, PyFile_WriteObject(NULL),
  PyErr_Restore with a non-traceback, DecodeUTF8Stateful `consumed` on error, module slots that
  contradict/repeat PyModuleDef fields, private `_PyUnicodeWriter_*` called with a public writer.

## Open issues (for the coordinator)
1. **pandas: heap corruption (use-after-free of a heap type)**. `import pandas` and small
   scripts work, and pandas modules collected alone are fine, but most pandas test
   subpackages segfault after some tens to hundreds of tests (per-subpackage run: api, config,
   construction, interchange, test_nanops, test_sorting, test_take, ... pass; apply,
   arithmetic, arrays, frame, groupby, indexes, series, ... crash). Collecting the whole suite
   in one process crashes in the cycle GC (garbage in the GC lists). Findings so far:
   - not the JIT (RPYTHON_JIT=0 crashes too), not the cycle GC (gc disabled crashes too), not
     the two IsUniquely*/IsUniqueReferencedTemporary changes (forcing both to 0 crashes too);
   - reproducer: `pytest --pyargs pandas.tests.test_multilevel` (seconds). gdb: crash in
     shape::clear_values <- subtype_dealloc of an instance (basicsize 104) whose *type object is
     already freed* (its tp_name points to reused memory, its ob_ref_shared is negative with the
     merged flags), reached from numpy's object-array clear (array_dealloc ->
     PyArray_ClearArray -> _Py_MergeZeroLocalRefcount -> dealloc). So a heap type's refcount
     underflows: some path decrefs a heap type once more than it increfs it (instances of
     heap types created by a C allocator path that does not incref the type, or a type
     reference stolen/borrowed wrongly in the C API). Next step: a debug build with an
     assertion in type dealloc that no live instance exists, or count incref/decref of heap
     types per allocation path (generic_alloc vs PyObject_Init/PyObject_GC_New vs
     tp_alloc of extension bases).
   Baseline pandas (same marker selection "not slow and not network and not db and not
   single_cpu"): 182,561 passed, 3,344 failed, 1,362 errors, 21,948 skipped in 35 min (many
   failures from missing optional dependencies) - the comparison waits on this crash.
2. **Hang in numpy test_hashtable concurrent tests** (about 1 run in 20, only under pytest): a
   ThreadPoolExecutor future is never completed while the workers sit idle in
   SimpleQueue.get (one worker thread missing). Repro: scratch hang.sh with
   `numpy._core.tests.test_hashtable -k concurrent`. Did not happen in the final full runs.
   Probably a runtime threading/refcount race; stability workstream.
3. **JIT (jit3)**: `PyUnstable_Object_IsUniqueReferencedTemporary` relies on (a) BINARY_OP
   operands being recorded by `interp::eval::binary_op_on_stack` (the interpreter and
   `jit::helpers::h_binary_op` call it; a tier-2 path that calls a type's nb_* slot directly
   must also call it or numpy just loses the optimisation), and (b) LoadFast taking a
   reference. If a tier starts borrowing locals onto the stack, the temporary check must also
   reject objects that a frame local holds, or numpy will write results into a live local.
   `PyUnstable_Object_IsUniquelyReferenced` discounts stack copies of the current frame's
   locals by scanning `Frame.slots` (stale slots above the stack top must stay NULL).
   Seen once: tier-2 panic "block without predecessor state" (jit/opt/build.rs:1002) while
   running test_capi.test_misc under regrtest; not reproduced alone.
4. Extension critical sections do not exclude Python-level operations of other threads on the
   same object (known design gap in notes/capi.md): bitarray's
   test_text_decoders_with_mutable_exporter fails sometimes.
5. msgspec test_wrong_classvar: msgspec continues with an exception set and CPython's
   type_new happens to notice it; our type_new does not look at a pending exception.
6. Sub-interpreters (`concurrent.interpreters`, `_interpreters`) are still missing.
7. test_capi: remaining failures are mostly behavioural (test_slots 117, test_run 88,
   test_float 31, test_watchers 34 - watchers do not fire, test_getargs 19, test_mem 16,
   test_bytes 15, test_codecs 14, test_exceptions 14, test_config 14, test_module 11, ...).

## Tools / scripts
- tools/wheels/run_numpy_tests.sh (updated as described above).
- Scratch scripts used for this round (not committed; in the session scratchpad numpy2/):
  runfull.sh (numpy full run), others.sh (bitarray/regex/markupsafe/msgspec/msgpack on
  ours/base), pdrun.sh (pandas per subpackage), capi_mods.sh / crashers.sh / gdbtest.sh
  (test_capi triage), hang.sh (hang catcher with gdb thread dump).
