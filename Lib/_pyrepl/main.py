"""Starting the pyrepl REPL (used by the interpreter's main program)."""

from __future__ import annotations

import errno
import os
import sys
import types

CAN_USE_PYREPL: bool
FAIL_REASON: str


def _check_usable() -> str:
    """Empty if the REPL can run on this terminal, else the reason."""
    try:
        fd = sys.stdin.fileno()
    except Exception as exc:
        return str(exc)
    if not os.isatty(fd):
        return str(OSError(errno.ENOTTY, "tty required", "stdin"))
    try:
        from .simple_interact import check
        return check()
    except Exception as exc:
        return str(exc) or repr(exc)


_reason = _check_usable()
CAN_USE_PYREPL = not _reason
FAIL_REASON = f"warning: can't use pyrepl: {_reason}" if _reason else ""
del _reason


def _run_startup(path: str, namespace: dict) -> None:
    sys.audit("cpython.run_startup", path)
    import tokenize
    with tokenize.open(path) as f:
        source = f.read()
    exec(compile(source, path, "exec"), namespace)


def interactive_console(mainmodule=None, quiet=False, pythonstartup=False):
    """Run the REPL in *mainmodule*'s namespace.

    *pythonstartup* is true (use $PYTHONSTARTUP) or the path of a startup
    file to run first."""
    if not CAN_USE_PYREPL:
        if FAIL_REASON and not os.getenv("PYTHON_BASIC_REPL"):
            print(FAIL_REASON, file=sys.stderr)
        baserepl = getattr(sys, "_baserepl", None)
        if baserepl is None:
            raise ImportError(FAIL_REASON or "pyrepl unavailable")
        return baserepl()

    if mainmodule is None:
        mainmodule = types.ModuleType("__main__")
    namespace = mainmodule.__dict__

    startup = None
    if isinstance(pythonstartup, str):
        startup = pythonstartup
    elif pythonstartup:
        startup = os.getenv("PYTHONSTARTUP")
    if startup:
        try:
            _run_startup(startup, namespace)
        except SystemExit:
            raise
        except BaseException:
            import traceback
            traceback.print_exc()

    if not hasattr(sys, "ps1"):
        sys.ps1 = ">>> "
    if not hasattr(sys, "ps2"):
        sys.ps2 = "... "

    from .console import InteractiveColoredConsole
    from .simple_interact import run_multiline_interactive_console

    console = InteractiveColoredConsole(namespace, filename="<stdin>")
    run_multiline_interactive_console(console)
