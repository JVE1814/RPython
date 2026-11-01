"""Read compiled terminfo entries and expand parameterized strings.

Only what the REPL needs from curses: ``TermInfo(term).get(name)`` (like
``curses.tigetstr``) and ``tparm(string, *params)``.  Compiled entries are
looked up in the usual directories; when none is found a small built-in
table is used (unless ``fallback=False``).
"""

from __future__ import annotations

import os
import struct

__all__ = ["TermInfo", "tparm", "ABSENT_STRING", "CANCELLED_STRING"]

# what tigetstr() gives for capabilities that are missing or cancelled
ABSENT_STRING = None
CANCELLED_STRING = None

_MAGIC_16BIT = 0o432
_MAGIC_32BIT = 0o1036

# String capability names in the order of the compiled terminfo format.
_STRING_NAMES = (
    "cbt bel cr csr tbc clear el ed hpa cmdch cup cud1 home civis cub1 "
    "mrcup cnorm cuf1 ll cuu1 cvvis dch1 dl1 dsl hd smacs blink bold smcup "
    "smdc dim smir invis prot rev smso smul ech rmacs sgr0 rmcup rmdc rmir "
    "rmso rmul flash ff fsl is1 is2 is3 if ich1 il1 ip kbs ktbc kclr kctab "
    "kdch1 kdl1 kcud1 krmir kel ked kf0 kf1 kf10 kf2 kf3 kf4 kf5 kf6 kf7 kf8 "
    "kf9 khome kich1 kil1 kcub1 kll knp kpp kcuf1 kind kri khts kcuu1 rmkx "
    "smkx lf0 lf1 lf10 lf2 lf3 lf4 lf5 lf6 lf7 lf8 lf9 rmm smm nel pad dch "
    "dl cud ich indn il cub cuf rin cuu pfkey pfloc pfx mc0 mc4 mc5 rep rs1 "
    "rs2 rs3 rf rc vpa sc ind ri sgr hts wind ht tsl uc hu iprog ka1 ka3 kb2 "
    "kc1 kc3 mc5p rmp acsc pln kcbt smxon rmxon smam rmam xonc xoffc enacs "
    "smln rmln kbeg kcan kclo kcmd kcpy kcrt kend kent kext kfnd khlp kmrk "
    "kmsg kmov knxt kopn kopt kprv kprt krdo kref krfr krpl krst kres ksav "
    "kspd kund kBEG kCAN kCMD kCPY kCRT kDC kDL kslt kEND kEOL kEXT kFND "
    "kHLP kHOM kIC kLFT kMSG kMOV kNXT kOPT kPRV kPRT kRDO kRPL kRIT kRES "
    "kSAV kSPD kUND rfi "
    + " ".join(f"kf{n}" for n in range(11, 64))
    + " el1 mgc smgl smgr fln sclk dclk rmclk cwin wingo hup dial qdial tone "
    "pulse hook pause wait u0 u1 u2 u3 u4 u5 u6 u7 u8 u9 op oc initc initp "
    "scp setf setb cpi lpi chr cvr defc swidm sdrfq sitm slm smicm snlq "
    "snrmq sshm ssubm ssupm sum rwidm ritm rlm rmicm rshm rsubm rsupm rum "
    "mhpa mcud1 mcub1 mcuf1 mvpa mcuu1 porder mcud mcub mcuf mcuu scs smgb "
    "smgbp smglp smgrp smgt smgtp sbim scsd rbim rcsd subcs supcs docr zerom "
    "csnm kmous minfo reqmp getm setaf setab pfxl devt csin s0ds s1ds s2ds "
    "s3ds smglr smgtb birep binel bicr colornm defbi endbi setcolor slines "
    "dispc smpch rmpch smsc rmsc pctrm scesc scesa ehhlm elhlm elohlm erhlm "
    "ethlm evhlm sgr1 slength OTi2 OTrs OTnl OTbc OTko OTma OTG2 OTG3 OTG1 "
    "OTG4 OTGR OTGL OTGU OTGD OTGH OTGV OTGC meml memu box1"
).split()

_ANSI = {
    "bel": b"\x07",
    "blink": b"\x1b[5m",
    "bold": b"\x1b[1m",
    "civis": b"\x1b[?25l",
    "clear": b"\x1b[H\x1b[2J",
    "cnorm": b"\x1b[?25h",
    "cr": b"\r",
    "cub": b"\x1b[%p1%dD",
    "cub1": b"\x08",
    "cud": b"\x1b[%p1%dB",
    "cud1": b"\n",
    "cuf": b"\x1b[%p1%dC",
    "cuf1": b"\x1b[C",
    "cup": b"\x1b[%i%p1%d;%p2%dH",
    "cuu": b"\x1b[%p1%dA",
    "cuu1": b"\x1b[A",
    "dch": b"\x1b[%p1%dP",
    "dch1": b"\x1b[P",
    "ed": b"\x1b[J",
    "el": b"\x1b[K",
    "home": b"\x1b[H",
    "hpa": b"\x1b[%i%p1%dG",
    "ich": b"\x1b[%p1%d@",
    "ich1": b"\x1b[@",
    "ind": b"\n",
    "kbs": b"\x7f",
    "kcub1": b"\x1bOD",
    "kcud1": b"\x1bOB",
    "kcuf1": b"\x1bOC",
    "kcuu1": b"\x1bOA",
    "kdch1": b"\x1b[3~",
    "kend": b"\x1bOF",
    "khome": b"\x1bOH",
    "kich1": b"\x1b[2~",
    "knp": b"\x1b[6~",
    "kpp": b"\x1b[5~",
    "kf1": b"\x1bOP",
    "kf2": b"\x1bOQ",
    "kf3": b"\x1bOR",
    "kf4": b"\x1bOS",
    "nel": b"\r\n",
    "rev": b"\x1b[7m",
    "ri": b"\x1bM",
    "rmkx": b"\x1b[?1l\x1b>",
    "sgr0": b"\x1b[m",
    "smkx": b"\x1b[?1h\x1b=",
    "smul": b"\x1b[4m",
    "rmul": b"\x1b[24m",
}

_DUMB = {
    "bel": b"\x07",
    "cr": b"\r",
    "cud1": b"\n",
    "ind": b"\n",
}

# built-in entries used when no compiled description is found
_TERMINAL_CAPABILITIES = {
    "ansi": _ANSI,
    "dumb": _DUMB,
    "xterm": _ANSI,
    "xterm-256color": _ANSI,
    "vt100": _ANSI,
    "linux": _ANSI,
    "screen": _ANSI,
    "tmux": _ANSI,
}


def _validate_terminal_name_or_raise(term) -> None:
    if not isinstance(term, str):
        raise TypeError(f"terminal name must be a string, not {type(term).__name__}")
    if not term:
        raise ValueError("terminal name must not be empty")
    if "\x00" in term:
        raise ValueError("terminal name must not contain NUL characters")
    if os.sep in term or (os.altsep and os.altsep in term) or "/" in term or term.startswith("."):
        raise ValueError(f"invalid terminal name {term!r}")


def _search_dirs() -> list[str]:
    dirs = []
    env = os.environ.get("TERMINFO")
    if env:
        dirs.append(env)
    home = os.path.expanduser("~")
    if home and home != "~":
        dirs.append(os.path.join(home, ".terminfo"))
    extra = os.environ.get("TERMINFO_DIRS")
    if extra:
        for d in extra.split(":"):
            dirs.append(d or "/usr/share/terminfo")
    dirs += ["/etc/terminfo", "/lib/terminfo", "/usr/share/terminfo",
             "/usr/lib/terminfo", "/usr/share/lib/terminfo",
             "/usr/local/share/terminfo", "/usr/local/lib/terminfo",
             "/opt/homebrew/share/terminfo", "/usr/share/misc/terminfo"]
    return dirs


def _find_entry(term: str) -> bytes | None:
    first = term[0]
    for d in _search_dirs():
        for sub in (first, f"{ord(first):02x}"):
            path = os.path.join(d, sub, term)
            try:
                with open(path, "rb") as f:
                    return f.read()
            except OSError:
                continue
    return None


def _cstring(table: bytes, offset: int) -> bytes:
    end = table.find(b"\x00", offset)
    if end < 0:
        end = len(table)
    return table[offset:end]


def _parse_entry(data: bytes) -> dict[str, bytes | None]:
    """String capabilities of a compiled entry (absent/cancelled -> None)."""
    if len(data) < 12:
        raise ValueError("terminfo entry too short")
    magic, names_size, nbool, nnum, nstr, str_size = struct.unpack("<6h", data[:12])
    if magic == _MAGIC_16BIT:
        num_size = 2
    elif magic == _MAGIC_32BIT:
        num_size = 4
    else:
        raise ValueError("not a compiled terminfo entry")
    pos = 12 + names_size + nbool
    if pos % 2:
        pos += 1
    pos += nnum * num_size
    offsets = struct.unpack(f"<{nstr}h", data[pos:pos + 2 * nstr])
    pos += 2 * nstr
    table = data[pos:pos + str_size]
    pos += str_size
    caps: dict[str, bytes | None] = {}
    for name, off in zip(_STRING_NAMES, offsets):
        caps[name] = None if off < 0 else _cstring(table, off)
    # extended (user-defined) capabilities
    if pos % 2:
        pos += 1
    if len(data) >= pos + 10:
        try:
            _parse_extended(data, pos, num_size, caps)
        except (struct.error, IndexError, ValueError):
            pass
    return caps


def _parse_extended(data: bytes, pos: int, num_size: int, caps: dict) -> None:
    nbool, nnum, nstr, _nitems, table_size = struct.unpack("<5h", data[pos:pos + 10])
    if min(nbool, nnum, nstr, table_size) < 0:
        return
    pos += 10 + nbool
    if pos % 2:
        pos += 1
    pos += nnum * num_size
    str_offsets = struct.unpack(f"<{nstr}h", data[pos:pos + 2 * nstr])
    pos += 2 * nstr
    nnames = nbool + nnum + nstr
    name_offsets = struct.unpack(f"<{nnames}h", data[pos:pos + 2 * nnames])
    pos += 2 * nnames
    table = data[pos:pos + table_size]
    # the string values come first in the table, then the names
    values = []
    end_of_values = 0
    for off in str_offsets:
        if off < 0:
            values.append(None)
            continue
        value = _cstring(table, off)
        values.append(value)
        end_of_values = max(end_of_values, off + len(value) + 1)
    names = [_cstring(table, end_of_values + off).decode("ascii", "replace")
             for off in name_offsets]
    string_names = names[nbool + nnum:]
    for name, value in zip(string_names, values):
        caps[name] = value


class TermInfo:
    def __init__(self, term: str | None, fallback: bool = True) -> None:
        if term is None:
            term = os.environ.get("TERM") or "dumb"
        _validate_terminal_name_or_raise(term)
        self.terminal_name = term
        self._caps: dict[str, bytes | None] = {}
        data = _find_entry(term)
        if data is not None:
            try:
                self._caps = _parse_entry(data)
                return
            except (ValueError, struct.error):
                if not fallback:
                    raise
        if term == "dumb":
            self._caps = dict(_DUMB)
            return
        if not fallback:
            raise ValueError(f"no terminfo entry for terminal {term!r}")
        self._caps = dict(_TERMINAL_CAPABILITIES.get(term, _ANSI))

    def get(self, cap: str) -> bytes | None:
        if not isinstance(cap, str):
            raise TypeError(f"capability name must be str, not {type(cap).__name__}")
        return self._caps.get(cap)


# --- parameterized strings ---------------------------------------------------------------

def _format_number(spec: str, value) -> bytes:
    """Apply a printf-style %[flags][width][.prec]{d,o,x,X,s} spec."""
    conv = spec[-1]
    flags = spec[:-1].lstrip(":")
    if conv == "s":
        text = value if isinstance(value, str) else (
            value.decode("latin-1") if isinstance(value, (bytes, bytearray)) else str(value))
        return ("%" + flags + "s").__mod__(text).encode("latin-1", "replace")
    if not isinstance(value, int):
        value = 0
    return ("%" + flags + conv).__mod__(value).encode("latin-1")


def tparm(cap: bytes, *params) -> bytes:
    """Expand the terminfo parameterized string *cap* with *params*."""
    if cap is None:
        raise TypeError("tparm() argument must be bytes, not None")
    if isinstance(cap, str):
        cap = cap.encode("latin-1")
    if not isinstance(cap, (bytes, bytearray)):
        raise TypeError(f"tparm() argument must be bytes, not {type(cap).__name__}")
    args = list(params) + [0] * (9 - len(params))
    out = bytearray()
    stack: list = []
    dynamic: dict[str, object] = {}
    static = _STATIC_VARS

    def pop():
        return stack.pop() if stack else 0

    def pop_int():
        v = pop()
        if isinstance(v, (bytes, bytearray, str)):
            return 0
        return int(v)

    s = bytes(cap)
    i = 0
    n = len(s)
    while i < n:
        c = s[i]
        if c != 0x25:  # '%'
            out.append(c)
            i += 1
            continue
        i += 1
        if i >= n:
            break
        op = chr(s[i])
        i += 1
        if op == "%":
            out.append(0x25)
        elif op == "c":
            v = pop_int()
            out.append((v & 0xFF) or 0x80)
        elif op == "s":
            v = pop()
            out += v if isinstance(v, (bytes, bytearray)) else str(v).encode("latin-1", "replace")
        elif op == "d":
            out += str(pop_int()).encode()
        elif op in "oxX":
            out += _format_number(op, pop_int())
        elif op == "p":
            idx = s[i] - 0x31
            i += 1
            stack.append(args[idx] if 0 <= idx < len(args) else 0)
        elif op == "P":
            name = chr(s[i])
            i += 1
            (static if name.isupper() else dynamic)[name] = pop()
        elif op == "g":
            name = chr(s[i])
            i += 1
            stack.append((static if name.isupper() else dynamic).get(name, 0))
        elif op == "'":
            stack.append(s[i])
            i += 2  # the character and the closing quote
        elif op == "{":
            end = s.index(b"}", i)
            stack.append(int(s[i:end] or b"0"))
            i = end + 1
        elif op == "l":
            v = pop()
            stack.append(len(v) if isinstance(v, (bytes, bytearray, str)) else 0)
        elif op in "+-*/m&|^=<>AO":
            b = pop_int()
            a = pop_int()
            if op == "+":
                r = a + b
            elif op == "-":
                r = a - b
            elif op == "*":
                r = a * b
            elif op == "/":
                r = int(a / b) if b else 0
            elif op == "m":
                r = (abs(a) % abs(b)) * (1 if a >= 0 else -1) if b else 0
            elif op == "&":
                r = a & b
            elif op == "|":
                r = a | b
            elif op == "^":
                r = a ^ b
            elif op == "=":
                r = int(a == b)
            elif op == "<":
                r = int(a < b)
            elif op == ">":
                r = int(a > b)
            elif op == "A":
                r = int(bool(a) and bool(b))
            else:
                r = int(bool(a) or bool(b))
            stack.append(r)
        elif op == "!":
            stack.append(int(not pop_int()))
        elif op == "~":
            stack.append(~pop_int())
        elif op == "i":
            if isinstance(args[0], int):
                args[0] += 1
            if isinstance(args[1], int):
                args[1] += 1
        elif op == "?":
            pass
        elif op == ";":
            pass
        elif op == "t":
            if not pop_int():
                i = _skip_branch(s, i, stop_at_else=True)
        elif op == "e":
            i = _skip_branch(s, i, stop_at_else=False)
        elif op in ":#0123456789. -":
            # printf-style formatting: %[:flags][width][.precision]conv
            j = i - 1
            while j < n and chr(s[j]) not in "doxXs":
                j += 1
            spec = s[i - 1:j + 1].decode("latin-1")
            i = j + 1
            if spec.endswith("s"):
                out += _format_number(spec, pop())
            else:
                out += _format_number(spec, pop_int())
        else:
            # unknown: keep it literally
            out += b"%" + op.encode("latin-1")
    return bytes(out)


_STATIC_VARS: dict[str, object] = {}


def _skip_branch(s: bytes, i: int, stop_at_else: bool) -> int:
    """Skip to after the matching %e (if *stop_at_else*) or %; ."""
    depth = 0
    n = len(s)
    while i < n:
        if s[i] != 0x25 or i + 1 >= n:
            i += 1
            continue
        op = chr(s[i + 1])
        i += 2
        if op == "?":
            depth += 1
        elif op == ";":
            if depth == 0:
                return i
            depth -= 1
        elif op == "e" and depth == 0 and stop_at_else:
            return i
    return i
