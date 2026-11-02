# Workstream tk: _tkinter, tkinter, idlelib

Branch: `worktree-agent-acb5abd1545c1e9ca` (commits prefixed `tk:`).

## Done

- `crates/runtime/src/modules/tty/tkinter.rs`: the `_tkinter` module, a
  binding to libtcl8.6/libtk8.6 loaded with `dl_api!` (no link-time
  dependency; ImportError if the libraries are missing).
  - types `_tkinter.tkapp`, `_tkinter.Tcl_Obj`, `_tkinter.tktimertoken`;
    `create()`, `_flatten`, `getbusywaitinterval`/`setbusywaitinterval`,
    `TclError`, `TCL_VERSION`/`TK_VERSION`, READABLE/WRITABLE/EXCEPTION,
    WINDOW_EVENTS/FILE_EVENTS/TIMER_EVENTS/IDLE_EVENTS/ALL_EVENTS/DONT_WAIT.
  - tkapp: call, eval, evalfile, record, adderrorinfo, globalsetvar/
    globalgetvar/globalunsetvar and set/get/unsetvar, getint/getdouble/
    getboolean, exprstring/exprlong/exprdouble/exprboolean, splitlist,
    createcommand/deletecommand, createfilehandler/deletefilehandler,
    createtimerhandler, mainloop/dooneevent/quit, interpaddr, loadtk,
    willdispatch, wantobjects modes 0/1/2.
  - Tcl_Obj <-> Python: str (modified UTF-8, surrogates and NUL), bytes,
    int (incl. bignums via decimal strings), float (NaN/inf read from the
    internal rep), bool, tuple/list, Tcl_Obj for other types; unregistered
    types (booleanString, bignum) are recognised by probing.
  - Threads: calls from other threads are queued to the interpreter
    thread with Tcl_ThreadQueueEvent and waited for ("main thread is not
    in main loop" when it is not running).  The GIL-free runtime blocks
    (threads::blocking) while waiting in Tcl; callbacks re-enter the VM.
  - Tcl's system encoding is set to utf-8 when the file system encoding
    is utf-8 (the runtime does not coerce the C locale).
- `Lib/tkinter/`: full package (`__init__`, constants, ttk, font,
  messagebox, filedialog, simpledialog, colorchooser, commondialog,
  dialog, dnd, scrolledtext, `__main__`).
- `Lib/idlelib/`: written from scratch (all modules used by the tests,
  plus pyshell/run/rpc startup, `__main__`, idle.py), config .def files,
  README.txt, News3.txt, CREDITS.txt, help.html (own text) and Icons/
  (own drawings, generated with tkinter under Xvfb).

## Test results (release build)

- test_tcl: 49 run / 4 skipped, OK (wantobjects 0, 1, 2).
- test_ttk_textonly: 13 run, OK.
- test_tkinter, test_ttk: skipped without the gui resource; with
  `-u gui` under Xvfb: 1424 run, all pass.
- test_idle (no display): **332 run / 88 skipped** (same counts as the
  baseline), 4 failing test methods (11 failures incl. subtests):
  - test_calltip test_builtins, test_signature_wrap: compare against
    CPython's docstrings of `object.__new__`/`type` and
    `textwrap.TextWrapper`; our runtime/stdlib docstrings are our own
    (D19), so these cannot pass without copying CPython text.
  - test_run test_get_message / test_get_multiple_message ('int.reel'):
    expect the 3.15 AttributeError hint "Did you mean '.real' instead of
    '.reel'?"; our traceback/excepthook print "Did you mean '.real'?"
    (runtime + Lib/traceback.py, not this workstream).
- test_idle with `-u gui` under Xvfb: run per module (see below); most
  modules pass.

## Missing / known problems

- `site` finds no LICENSE file in our installation, so `license()` has
  one line and help_about's printer test (gui only) fails; the installer
  should ship a LICENSE.txt next to the stdlib.
- tkinter with wantobjects=0: Text.window_cget('padx') returns a str
  where the test expects an int (one ttk test as well).
- IDLE GUI behaviour is implemented to the tests and common use; the
  module browser, debugger and shell were exercised through the tests
  only.  ShellSidebar redraws all prompts on each change (fine for
  normal shells, slow for huge outputs).

## Decisions

- No external crates; Tcl/Tk loaded at run time (so the interpreter
  builds and runs without Tk installed).
- idlelib is our own design, matching the names and behaviour the tests
  use; help texts, messages and docs are newly written (D19).
- `Lib/idlelib/idle_test` is a dev copy of `spec/Lib/idlelib/idle_test`
  and is never committed (snapshot.sh installs it, D23).
