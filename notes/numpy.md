# Workstream "numpy": binary cp315t wheels on our interpreter

Branch: `worktree-agent-a1d1b4c37f27f2c53`. The project paused before the full test-suite runs
were finished; this file records the state and the next steps.

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
- Master still lacks `xmlrpc` (pip's vendored distlib imports xmlrpc.client). It is on the
  markup2 branch. For the experiment, Lib/xmlrpc from commit 623bc71 was copied into the
  snapshot only, not committed. Merge `core` or wait for markup2 before running the script
  from master.

## Numbers so far
- Baseline CPython 3.15.2t with the same wheels (numpy 2.5.4, pytest 9.2.0, hypothesis
  6.99.9): the full run was still in progress at the pause. At 36% of the suite it had
  18,789 PASSED and 0 FAILED. The log is at /tmp/np/base/full.log.
- Ours: the full suite was not run yet. Before fix 3, every numpy test module crashed
  while importing numpy.random (conftest/test imports). After fixes 1-3 the import and the
  traceback paths work. The full-suite run is the next step.

## Next steps
1. Run `tools/wheels/run_numpy_tests.sh -w <wheeldir> -B`. Then go through
   `only_ours.txt`, crashes first (the -v log names the test that was running).
2. Other cp315t wheels to test next (already downloaded to /tmp/np/wheels): regex
   (tests ship as regex/tests/test_regex.py), bitarray (bitarray/test_*.py, including
   test_free_threading.py), pandas 3.1.0 (Cython-heavy), msgspec, markupsafe.
   pydantic-core and cffi are on PyPI too.
3. `PyUnstable_Object_IsUniqueReferencedTemporary` is `refcount == 1` (numpy uses it to
   decide when it may reuse a temporary array in place). This is correct while every tier
   increfs on LOAD_FAST (tier 0 and tier 1 both do). If a JIT tier starts borrowing locals,
   this function must check that the object is actually on the current frame's value
   stack.
4. Possibly: `types.CodeType(...)` (tp_new on code) through `new_code_from_c`, for
   limited-API Cython modules.
