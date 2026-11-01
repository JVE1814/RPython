# Workstream "tty" — notes

Branch: `worktree-agent-a9c7f76de33430c27`. All Rust code lives in
`crates/runtime/src/modules/tty/` (one line each in `modules/mod.rs`: `pub mod tty;` and
`tty::register();`).

## Decision: runtime dlopen of the system libraries (D20)
`tty/dl.rs` provides `dl_api!`, which declares a struct of C function pointers (plus
data-symbol addresses such as `stdscr`, `LINES`, `acs_map`, `rl_line_buffer`) resolved with
`dlopen`/`dlsym` on first import and cached in a `OnceLock`. Consequences:
* building the interpreter needs no -dev packages and no link flags;
* a missing library (or symbol) makes the module raise `ImportError`, exactly like a
  CPython module that was not built; optional symbols are `?fn` (`Option<fn>`);
* libraries are opened `RTLD_GLOBAL` so libpanelw resolves against libncursesw.
Libraries: libgdbm.so.6, libgdbm_compat.so.4, libreadline.so.8, libncursesw.so.6,
libpanelw.so.6 (Tcl/Tk not bound yet).

## Modules
* `_gdbm`, `_dbm` (tty/dbm.rs): one shared mapping type implementation, two backends.
  `_dbm.library == "GNU gdbm"` (ndbm emulation of gdbm_compat: NAME.pag + NAME.dir).
  Errors are subclasses of OSError; open errors carry errno/filename.
* `Lib/dbm/`: `__init__.py` and `dumb.py` were rewritten clean-room (D19); `gnu.py`,
  `ndbm.py`, `sqlite3.py` new. dbm.sqlite3 imports `sqlite3`, which master does not have
  yet, so test_dbm_sqlite3 is skipped and test_dbm runs 55 instead of 72 tests until the
  "bindings" sqlite3 lands. Disclosure: before the coordinator's clarification I had looked
  at the old `dbm/__init__.py` (not dumb.py); the new one has its own structure.
* `readline` (tty/readline.rs): full API incl. completion hooks (begidx/endidx are character
  offsets), display-matches hook, startup/pre-input hooks, get_pre_input_hook, history
  files with truncation, set_auto_history, `backend = "readline"`. Importing it installs
  `PyOS_ReadlineFunctionPointer`; the read loop uses readline's callback API + select() so
  Ctrl-C raises KeyboardInterrupt; a mutex serialises library use across threads.
* `input()` (builtins.rs, 3 lines → tty/input.rs): when the hook is installed and
  sys.stdin/sys.stdout are fds 0/1 and terminals, the prompt goes through PyOS_Readline
  (stdout encoding for the prompt, stdin encoding for the result) like CPython.
* `Lib/rlcompleter.py`: clean-room. Class attributes that are properties/member/getset or
  other non-callable descriptors are completed without evaluating them.
* Small runtime fixes needed by rlcompleter: text signatures for `NoneType()`,
  `object.__init_subclass__($cls, /)` and `type.mro($self, /)` (none.rs, object_type.rs).
* `_curses` (tty/curses.rs) + `_curses_panel` (tty/panel.rs) + `Lib/curses/`
  (`__init__`, `ascii`, `has_key`, `panel`, `textpad`, all clean-room). Wide build: str
  characters/strings use the wide-character API after checking that the window encoding can
  encode them; bytes and ints go through the chtype API. `in_wch()` returns the cell's
  text as str. `ncurses_version` is a structseq. ACS_* and LINES/COLS appear after
  initscr() (also mirrored into the `curses` package by update_lines_cols/resizeterm).
* REPL (modules/pymain.rs): runs `sys.__interactivehook__`; on a terminal lines are read
  through PyOS_Readline (line editing, history, tab completion via site/rlcompleter);
  interactive sources are registered with `linecache._register_code` so tracebacks show
  them; a final line without newline (EOF) emits "\n" first. `fancy_repl()` is the hook
  point for `_pyrepl.main.interactive_console(main, quiet, pythonstartup=...)` (used when
  stdin/stdout are ttys and PYTHON_BASIC_REPL is unset; falls back silently on ImportError).

## Test status (release build, `-m test`)
| test | result |
|---|---|
| test_dbm_gnu | 19/19 pass (baseline 19) |
| test_dbm_ndbm | 16/16 pass (baseline 16) |
| test_dbm_dumb | 28/28 pass |
| test_dbm | 55 run, 5 skipped (sqlite3 missing in master; baseline 72/3) |
| test_dbm_sqlite3 | skipped: no `sqlite3` module yet |
| test_readline | 16 run, 1 skipped (non-UTF-8 locale skip, same count as baseline) |
| test_rlcompleter | 11/11 pass |
| test_curses | skipped by default (needs `-u curses`); with `-u curses` under a pty: 113/113 pass, 0 skipped; without a pty: 113 run, 4 skipped |
| test_repl | 9 of the 12 non-asyncio tests pass; asyncio REPL tests fail because `asyncio/__main__.py` does not exist (asyncio is owned by another workstream); test_runsource_show_syntax_error_location still differs in the number of prompt lines |

## Not done
* `_pyrepl` (the 3.13+ fancy REPL) — not written; the launcher hook is in place.
  test_pyrepl therefore still skips (no `_pyrepl`).
* `_tkinter` / `tkinter` / idlelib — not started (test_tcl, test_tkinter, test_ttk*,
  test_idle skip on import of `_tkinter`). Plan: bind libtcl8.6/libtk8.6 the same way
  (dl_api!), Tcl_Obj <-> Python conversion per test_tcl (wantobjects levels, bytes via
  bytearray objects, modified UTF-8/surrogates), createcommand trampolines, then a
  clean-room tkinter package.
* curses/readline/dbm objects are not internally locked (no GIL): concurrent use of one
  window/database from several threads is unsynchronised (readline itself is serialised).

## Tooling
To run a test under a pseudo-terminal without `script`: a small driver using
os.openpty/fork/setsid (our interpreter) — `os.login_tty` is missing in posix (osio).
