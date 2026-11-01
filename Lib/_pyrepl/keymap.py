"""Key specifications and keymap trees.

A key specification is a string in a small emacs-like notation:

* a plain character stands for itself;
* ``\\C-x`` is control-x, ``\\M-x`` is meta-x (ESC followed by x);
* ``\\<name>`` names a special key, e.g. ``\\<up>`` or ``\\<page down>``;
* ``\\n``, ``\\t``, ``\\e`` ... are the usual escapes.

``parse_keys`` turns a specification into the list of key names the event
queue produces; ``compile_keymap`` turns a flat {bytes: value} mapping into
a tree keyed by single bytes, which the event queues walk byte by byte.
"""

from __future__ import annotations


class KeySpecError(Exception):
    pass


_escapes = {
    "\\": "\\",
    "'": "'",
    '"': '"',
    "a": "\a",
    "b": "\b",
    "e": "\033",
    "f": "\f",
    "n": "\n",
    "r": "\r",
    "t": "\t",
    "v": "\v",
}

_keynames = {
    "backspace": "backspace",
    "delete": "delete",
    "down": "down",
    "end": "end",
    "enter": "\r",
    "escape": "\033",
    "home": "home",
    "insert": "insert",
    "left": "left",
    "page down": "page down",
    "page up": "page up",
    "return": "\r",
    "right": "right",
    "space": " ",
    "tab": "\t",
    "up": "up",
}
for _n in range(1, 21):
    _keynames[f"f{_n}"] = f"f{_n}"


def _parse_one(spec: str, i: int) -> tuple[str | list[str], int]:
    """Parse the key starting at spec[i]; return (key or [keys], next index).

    Running off the end of *spec* raises IndexError on purpose (an
    incomplete specification)."""
    c = spec[i]
    if c != "\\":
        return c, i + 1
    i += 1
    c = spec[i]
    lower = c.lower()
    if lower in ("c", "m"):
        letter = lower.upper()
        if spec[i + 1] != "-":
            raise KeySpecError(f"\\{letter} must be followed by `-' "
                               f"(char {i + 1} of {spec!r})")
        i += 2
        if (spec[i] == "\\" and spec[i + 1].lower() == lower
                and spec[i + 2] == "-"):
            raise KeySpecError(f"doubled \\{letter}- (char {i} of {spec!r})")
        inner, i = _parse_one(spec, i)
        if letter == "M":
            rest = inner if isinstance(inner, list) else [inner]
            return ["\033", *rest], i
        if isinstance(inner, list) or len(inner) != 1:
            raise KeySpecError(f"\\C- followed by invalid key "
                               f"(char {i} of {spec!r})")
        if inner == "?":
            return "\x7f", i
        return chr(ord(inner) & 0x1F), i
    if c == "<":
        close = spec.find(">", i)
        if close < 0:
            raise KeySpecError(f"unterminated \\< starting at char {i} of {spec!r}")
        name = spec[i + 1:close].lower()
        if name not in _keynames:
            raise KeySpecError(f"unrecognised keyname `{name}' at char {i} of {spec!r}")
        return _keynames[name], close + 1
    if c in _escapes:
        return _escapes[c], i + 1
    if c.isdigit():
        # octal escape: \NNN
        j = i
        while j < len(spec) and j < i + 3 and spec[j] in "01234567":
            j += 1
        if j > i:
            return chr(int(spec[i:j], 8)), j
    raise KeySpecError(f"unknown backslash escape {c!r} at char {i} of {spec!r}")


def parse_keys(keys: str) -> list[str]:
    """Translate a key specification into the list of key names."""
    out: list[str] = []
    i = 0
    while i < len(keys):
        key, i = _parse_one(keys, i)
        if isinstance(key, list):
            out.extend(key)
        else:
            out.append(key)
    return out


def _add(tree: dict, key: bytes, value, empty: bytes) -> None:
    if not key:
        if empty in tree:
            raise KeySpecError(f"key definitions clash at {key!r}")
        tree[empty] = value
        return
    head, tail = key[:1], key[1:]
    if tail:
        sub = tree.setdefault(head, {})
        if not isinstance(sub, dict):
            raise KeySpecError(f"key definitions for {head!r} clash")
        _add(sub, tail, value, empty)
    else:
        if head in tree:
            raise KeySpecError(f"key definitions for {head!r} clash")
        tree[head] = value


def compile_keymap(keymap: dict, empty: bytes = b"") -> dict:
    """Turn {byte sequence: value} into a tree keyed by single bytes.

    Values that are dicts are compiled recursively; a key that is both a
    complete sequence and a prefix of another raises KeySpecError."""
    tree: dict = {}
    for key, value in keymap.items():
        if not isinstance(key, bytes):
            raise TypeError(f"keymap keys must be bytes, not {type(key).__name__}")
        if isinstance(value, dict):
            value = compile_keymap(value, empty)
        _add(tree, key, value, empty)
    return tree
