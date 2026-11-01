"""Key events from a Unix terminal: key sequences come from terminfo plus
the common VT100/xterm sequences."""

from __future__ import annotations

import os
from termios import tcgetattr, VERASE

from .base_eventqueue import BaseEventQueue
from . import terminfo

# terminfo capability -> key name
TERMINAL_KEYNAMES = {
    "delete": "kdch1",
    "down": "kcud1",
    "end": "kend",
    "enter": "kent",
    "home": "khome",
    "insert": "kich1",
    "left": "kcub1",
    "page down": "knp",
    "page up": "kpp",
    "right": "kcuf1",
    "up": "kcuu1",
}
for _i in range(1, 21):
    TERMINAL_KEYNAMES[f"f{_i}"] = f"kf{_i}"

# sequences sent by most terminals whatever their terminfo entry says
COMMON_SEQUENCES = {
    b"\x1b[A": "up",
    b"\x1b[B": "down",
    b"\x1b[C": "right",
    b"\x1b[D": "left",
    b"\x1b[H": "home",
    b"\x1b[F": "end",
    b"\x1bOA": "up",
    b"\x1bOB": "down",
    b"\x1bOC": "right",
    b"\x1bOD": "left",
    b"\x1bOH": "home",
    b"\x1bOF": "end",
    b"\x1b[1~": "home",
    b"\x1b[2~": "insert",
    b"\x1b[3~": "delete",
    b"\x1b[4~": "end",
    b"\x1b[5~": "page up",
    b"\x1b[6~": "page down",
    b"\x1b[7~": "home",
    b"\x1b[8~": "end",
    b"\x1bOP": "f1",
    b"\x1bOQ": "f2",
    b"\x1bOR": "f3",
    b"\x1bOS": "f4",
    b"\x7f": "backspace",
}


def get_terminal_keycodes(ti: terminfo.TermInfo) -> dict[bytes, str]:
    codes = {}
    for key, cap in TERMINAL_KEYNAMES.items():
        seq = ti.get(cap)
        if seq:
            codes[seq] = key
    return codes


def _without_clashes(mapping: dict[bytes, str]) -> dict[bytes, str]:
    """Drop sequences that are prefixes of others (or have them as
    prefixes); the first one added wins."""
    kept: dict[bytes, str] = {}
    for seq, name in mapping.items():
        if any(seq.startswith(other) or other.startswith(seq) for other in kept):
            continue
        kept[seq] = name
    return kept


class EventQueue(BaseEventQueue):
    def __init__(self, fd: int, encoding: str, ti: terminfo.TermInfo) -> None:
        keycodes = dict(COMMON_SEQUENCES)
        keycodes.update(get_terminal_keycodes(ti))
        if os.isatty(fd):
            try:
                erase = tcgetattr(fd)[6][VERASE]
                if isinstance(erase, int):
                    erase = bytes([erase])
                keycodes[erase] = "backspace"
            except Exception:
                pass
        super().__init__(encoding, _without_clashes(keycodes))
