"""Paging long text to the terminal (used by help() and the history
viewer)."""

from __future__ import annotations

import io
import os
import re
import sys

# A pager takes the text and an optional title.
_BACKSPACE_OVERSTRIKE = re.compile(".\b")


def plain(text: str) -> str:
    """*text* without the backspace overstrike used for bold."""
    return _BACKSPACE_OVERSTRIKE.sub("", text)


def get_pager():
    """Choose the best way to page text on this system."""
    if not hasattr(sys.stdin, "isatty") or not hasattr(sys.stdout, "isatty"):
        return plain_pager
    try:
        if not sys.stdin.isatty() or not sys.stdout.isatty():
            return plain_pager
    except (ValueError, AttributeError):
        return plain_pager
    if sys.platform == "emscripten":
        return plain_pager
    use_pager = os.environ.get("MANPAGER") or os.environ.get("PAGER")
    if use_pager:
        if sys.platform == "win32":
            return lambda text, title="": tempfile_pager(plain(text), use_pager)
        if os.environ.get("TERM") in ("dumb", "emacs"):
            return lambda text, title="": pipe_pager(plain(text), use_pager, title)
        return lambda text, title="": pipe_pager(text, use_pager, title)
    if os.environ.get("TERM") in ("dumb", "emacs"):
        return plain_pager
    if sys.platform == "win32":
        return lambda text, title="": tempfile_pager(plain(text), "more <")
    if hasattr(os, "system") and os.system("(pager) 2>/dev/null") == 0:
        return lambda text, title="": pipe_pager(text, "pager", title)
    if hasattr(os, "system") and os.system("(less) 2>/dev/null") == 0:
        return lambda text, title="": pipe_pager(text, "less", title)
    return tty_pager


def _escape_less(s: str) -> str:
    return re.sub(r"([?:.%\\])", r"\\\1", s)


def plain_pager(text: str, title: str = "") -> None:
    """Write *text* to stdout without paging."""
    sys.stdout.write(plain(_escape_stdout(text)))


def _escape_stdout(text: str) -> str:
    encoding = getattr(sys.stdout, "encoding", None) or "utf-8"
    return text.encode(encoding, "backslashreplace").decode(encoding)


def pipe_pager(text: str, cmd: str, title: str = "") -> None:
    """Page *text* through the shell command *cmd*."""
    import subprocess
    env = os.environ.copy()
    if title:
        title += " "
    prompt = f"{_escape_less(title)}line %lt?L/%L.:"
    env["LESS"] = f"-RmPm{prompt}$PM{prompt}$"
    proc = subprocess.Popen(cmd, shell=True, stdin=subprocess.PIPE,
                            errors="backslashreplace", env=env)
    assert proc.stdin is not None
    try:
        with proc.stdin as pipe:
            try:
                pipe.write(text)
            except KeyboardInterrupt:
                pass
    except OSError:
        pass
    while True:
        try:
            proc.wait()
            break
        except KeyboardInterrupt:
            pass


def tempfile_pager(text: str, cmd: str, title: str = "") -> None:
    """Page *text* by running *cmd* on a temporary file."""
    import tempfile
    with tempfile.TemporaryDirectory() as tempdir:
        filename = os.path.join(tempdir, "pydoc.out")
        with open(filename, "w", errors="backslashreplace",
                  encoding=os.device_encoding(0) if sys.platform == "win32" else None) as f:
            f.write(text)
        os.system(cmd + ' "' + filename + '"')


def _getch(fd) -> str:
    return os.read(fd, 1).decode(errors="replace")


def tty_pager(text: str, title: str = "") -> None:
    """Page *text* on the terminal itself, a screenful at a time."""
    lines = plain(_escape_stdout(text)).split("\n")
    try:
        import tty
        import termios
        fd = sys.stdin.fileno()
        old = termios.tcgetattr(fd)
        tty.setcbreak(fd)
        has_tty = True
    except (ImportError, AttributeError, io.UnsupportedOperation, OSError):
        has_tty = False
        fd = None
        old = None
    try:
        try:
            height = int(os.environ.get("LINES", 0))
        except ValueError:
            height = 0
        if height <= 1:
            height = 25
        page = height - 1
        sys.stdout.write("\n".join(lines[:page]) + "\n")
        shown = page
        while shown < len(lines):
            sys.stdout.write("-- more --")
            sys.stdout.flush()
            key = _getch(fd) if has_tty else sys.stdin.read(1)
            if key in ("q", "Q"):
                sys.stdout.write("\r          \r")
                break
            if key in ("\r", "\n"):
                sys.stdout.write("\r          \r" + lines[shown] + "\n")
                shown += 1
                continue
            if key in ("b", "B", "\x1b"):
                shown = max(shown - page - page, page)
            sys.stdout.write("\n" + "\n".join(lines[shown:shown + page]) + "\n")
            shown += page
    finally:
        if has_tty:
            termios.tcsetattr(fd, termios.TCSAFLUSH, old)
