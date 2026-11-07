"""Terminal handling with curses.

The low-level functions and constants come from the _curses extension; this
package adds the pieces that need Python-level state: initscr() and
start_color() publish the values only known after the screen (or the color
system) has been initialised, and wrapper() runs a function inside a curses
session and always restores the terminal.
"""

import os as _os
import sys as _sys

from _curses import *
import _curses

__all__ = [name for name in dir(_curses) if not name.startswith("_")]
__all__ += [n for n in ("initscr", "start_color", "wrapper", "has_key", "window") if n not in __all__]


def _publish(predicate):
    """Copy the _curses globals selected by *predicate* into this package."""
    here = globals()
    for name, value in vars(_curses).items():
        if predicate(name):
            here[name] = value


def initscr():
    """Initialise the screen and return the window for the whole screen.

    The terminal description is loaded first so that a bad $TERM raises
    curses.error instead of terminating the process.
    """
    term = _os.environ.get("TERM", "unknown")
    out = _sys.__stdout__
    setupterm(term=term, fd=out.fileno() if out is not None else 1)
    screen = _curses.initscr()
    _publish(lambda name: name.startswith("ACS_") or name in ("LINES", "COLS"))
    return screen


def start_color():
    """Initialise colors and publish COLORS and COLOR_PAIRS."""
    result = _curses.start_color()
    _publish(lambda name: name in ("COLORS", "COLOR_PAIRS"))
    return result


try:
    has_key
except NameError:
    from .has_key import has_key


def wrapper(func, /, *args, **kwds):
    """Run func(stdscr, *args, **kwds) in a curses session.

    The terminal is put into cbreak/noecho mode with keypad translation (and
    colors when available); whatever happens inside *func*, the original
    terminal state is restored before returning or propagating the error.
    """
    screen = None
    try:
        screen = initscr()
        noecho()
        cbreak()
        screen.keypad(1)
        try:
            start_color()
        except error:
            pass
        return func(screen, *args, **kwds)
    finally:
        if screen is not None:
            screen.keypad(0)
            echo()
            nocbreak()
            endwin()
