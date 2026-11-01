# Workstream "tty2" — notes

Branch: `worktree-agent-a62e58e2d50a12844` (contains the "tty" branch and master as of the pause).

## Done
* `os.login_tty`, `posix._is_inputhook_installed`, `posix._inputhook` (posix/fs.rs);
  NOARGS posix functions got `name($module, /)` text signatures (so rlcompleter
  completes `os.getpid()` with the closing paren).
* `Lib/_pyrepl/` written from scratch (rule 9): utils (widths, syntax colouring via
  tokenize), keymap, input (KeymapTranslator), render (cell model + line diff),
  content/layout (wrapping, pos<->xy), console (Console ABC, Event,
  InteractiveColoredConsole running each statement separately), commands, reader,
  historical_reader (history, isearch, prefix search), completing_reader (menu,
  completion "actions"), readline (ReadlineAlikeReader, auto-indent Enter,
  _ReadlineWrapper + readline-compatible module API, `_setup`), fancycompleter,
  _module_completer (ImportParser, ModuleCompleter, HARDCODED_SUBMODULES),
  terminfo (pure-Python compiled-terminfo reader + tparm, fallback tables),
  fancy_termios, base/unix eventqueue, unix_console (diffing renderer, ich1/dch1,
  scrolling with ri/ind, input hook in cooked mode), simple_interact (REPL loop,
  exit/quit/help/clear/copyright commands, incremental history file append),
  main (CAN_USE_PYREPL/FAIL_REASON, interactive_console), pager, _threading_handler,
  __main__.
* pymain.fancy_repl: creates `__main__` first, checks `_pyrepl.main.CAN_USE_PYREPL`
  (prints FAIL_REASON, falls back to the basic REPL), prints the banner, runs
  `sys.__interactivehook__`, then `interactive_console(main, quiet, pythonstartup=path)`.

## Test status (release build, after merging master)
test_pyrepl: 364 run, 24 failures, 44 skipped (baseline: 364/1/44; first run was 33 failures/53 skipped).
Known remaining causes:
* missing stdlib in this branch, not pyrepl: `importlib.resources`/`importlib.metadata`
  (pkgtools), `__phello__` frozen package, `types.LazyImportType` (lazy imports),
  `concurrent.futures` (fixed by the master merge?).
* TestMain pty tests were all skipped/failing because `__main__` did not exist when
  `_pyrepl.main` was imported (fixed in pymain, needs a rerun).
* fancycompleter colours: tests expect `theme = _colorize.get_theme()` (not forced) and
  `ANSIColors.RESET` always appended by `_color_for_obj` — not changed yet.
* InteractiveColoredConsole.showsyntaxerror should fill `exc.text` from `source=` for
  symtable errors (test_runsource_show_syntax_error_location).
* ModuleCompleter: when a module is already imported but a sys.path search finds a
  different location first, propose no submodules (test_already_imported_custom_*);
  current code uses the imported module's own path.
* test_control_character / test_setpos_from_xy_for_non_printing_char /
  test_control_characters: control-char display rules in utils.disp_char need checking.
* unix_console test_cursor_back_write (ich1 insert path) to debug.

## Not done (next steps)
* site.register_readline should use `_pyrepl.readline` for history when pyrepl is usable
  (read history, atexit write, set `_pyrepl.readline._wrapper.history_file` for the
  incremental append) and must not import `_pyrepl` under PYTHON_BASIC_REPL.
* pdb `_pyrepl_available()` / PdbPyReplInput (pdb owned by devtools).
* windows_console/windows_eventqueue (not needed on Linux).
* `_tkinter` + `Lib/tkinter/` not started. Baseline (py-default, Tk 8.6, no DISPLAY):
  test_tcl 49 run/4 skipped, test_ttk_textonly 13 run, test_tkinter/test_ttk skip
  ("gui" resource), test_idle 332 run/88 skipped — but idlelib's tests live in
  idlelib/idle_test (not in spec), so test_idle is out of reach without writing idlelib
  and its tests. Plan: dlopen libtcl8.6/libtk8.6 via dl_api! (headers in /usr/include/tcl
  give Tcl_Obj/Tcl_ObjType layouts), Xvfb is available for GUI testing.
* Per-object locking for curses windows / dbm handles not started.
* Note: creating `_pyrepl` un-skips force_colorized tests across the suite
  (test.support.force_color requires `_pyrepl`).
