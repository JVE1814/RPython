"""A completer that colours candidates by the type of their value.

Behaves like ``rlcompleter.Completer`` with three differences: several
candidates sharing a prefix longer than what was typed collapse into that
prefix (so the first Tab extends the text), candidates are coloured with
the theme's ``fancycompleter`` section, and attribute values are looked up
without running properties or resolving lazy imports.
"""

from __future__ import annotations

import builtins
import keyword
import re
import types

import _colorize
import rlcompleter

__all__ = ["Completer", "colorize_matches", "commonprefix"]

_MISSING = object()
_EXPR_ATTR = re.compile(r"(\S+?)\.(\w*)$", re.DOTALL)


def commonprefix(names: list[str]) -> str:
    """Longest common leading text of *names* ('' for no names)."""
    if not names:
        return ""
    low, high = min(names), max(names)
    i = 0
    limit = min(len(low), len(high))
    while i < limit and low[i] == high[i]:
        i += 1
    return low[:i]


def _section_color(value, section) -> str:
    if isinstance(value, bool):
        return section.bool
    if value is None:
        return section.none
    if isinstance(value, types.ModuleType):
        return section.module
    if isinstance(value, type):
        if issubclass(value, BaseException):
            return section.exception
        return section.type
    if isinstance(value, str):
        return section.str
    if isinstance(value, (bytes, bytearray)):
        return section.bytes
    if isinstance(value, int):
        return section.int
    if isinstance(value, float):
        return section.float
    if isinstance(value, complex):
        return section.complex
    if isinstance(value, types.FunctionType):
        return section.function
    if isinstance(value, types.BuiltinFunctionType):
        owner = getattr(value, "__self__", None)
        if owner is None or isinstance(owner, types.ModuleType):
            return section.builtin_function
        return section.builtin_method
    if isinstance(value, types.MethodType):
        return section.method
    if isinstance(value, (types.MethodWrapperType, types.WrapperDescriptorType,
                          types.MethodDescriptorType, types.ClassMethodDescriptorType)):
        return section.builtin_method
    return ""


def _color_for_obj(name: str, value, theme) -> str:
    color = _section_color(value, theme.fancycompleter)
    if not color:
        return name
    return f"{color}{name}{theme.fancycompleter.reset}"


def colorize_matches(names, values, theme):
    """Colour each of *names* by the type of the matching value."""
    if theme is None:
        theme = _colorize.get_theme(force_color=True)
    return [_color_for_obj(name, value, theme) for name, value in zip(names, values)]


def _static_lookup(obj, name):
    """The attribute *name* of *obj*, without triggering properties or
    lazy imports when that can be avoided."""
    if isinstance(obj, types.ModuleType):
        try:
            return obj.__dict__[name]
        except (KeyError, AttributeError, TypeError):
            pass
    try:
        mro = type.__getattribute__(type(obj), "__mro__")
    except Exception:
        mro = ()
    for klass in mro:
        try:
            attr = klass.__dict__[name]
        except (KeyError, AttributeError, TypeError):
            continue
        if isinstance(attr, property):
            return attr
        break
    try:
        return getattr(obj, name)
    except Exception:
        return _MISSING


class Completer(rlcompleter.Completer):
    def __init__(self, namespace=None, *, use_colors="auto",
                 consider_getitems=True) -> None:
        super().__init__(namespace)
        if use_colors == "auto":
            use_colors = _colorize.can_colorize()
        self.use_colors = bool(use_colors)
        self.theme = _colorize.get_theme(force_color=True) if self.use_colors else None
        self.consider_getitems = consider_getitems

    def complete(self, text, state):
        if not text.strip():
            return None
        return super().complete(text, state)

    # -- shaping the result ---------------------------------------------------------

    def _finish(self, text: str, names: list[str], values: list, single_postfix=True):
        if not names:
            return []
        if len(names) == 1:
            name = names[0]
            value = values[0]
            if single_postfix and value is not _MISSING:
                name = self._callable_postfix(value, name)
            return [name]
        common = commonprefix(names)
        if len(common) > len(text):
            return [common]
        order = sorted(range(len(names)), key=names.__getitem__)
        names = [names[i] for i in order]
        values = [values[i] for i in order]
        if self.use_colors:
            return [_color_for_obj(n, None if v is _MISSING else v, self.theme)
                    if v is not _MISSING else n
                    for n, v in zip(names, values)]
        return names

    def global_matches(self, text):
        seen = {"__builtins__"}
        names = []
        values = []
        for word in keyword.kwlist + keyword.softkwlist:
            if word.startswith(text) and word not in seen:
                seen.add(word)
                names.append(word)
                values.append(_MISSING)
        for source in (self.namespace, builtins.__dict__):
            for word, value in list(source.items()):
                if word.startswith(text) and word not in seen:
                    seen.add(word)
                    names.append(word)
                    values.append(value)
        return self._finish(text, names, values)

    def attr_matches(self, text):
        m = _EXPR_ATTR.match(text)
        if m is None:
            return []
        expr, attr = m.group(1), m.group(2)
        if "(" in expr or ")" in expr:
            return []
        if not self.consider_getitems and ("[" in expr or "]" in expr):
            return []
        # Completing attributes means evaluating the expression before the
        # dot in the user's own namespace, exactly like rlcompleter does;
        # calls are refused above so only lookups/subscripts run.
        try:
            obj = eval(expr, self.namespace)
        except Exception:
            return []
        try:
            words = set(dir(obj))
        except Exception:
            words = set()
        words.discard("__builtins__")
        if hasattr(obj, "__class__"):
            words.add("__class__")
            try:
                words.update(dir(type(obj)) if isinstance(obj, type) else ())
            except Exception:
                pass
            if isinstance(obj, type):
                words.update(n for n in ("mro",) if hasattr(type(obj), n))
        if attr == "":
            noprefix = "_"
        elif attr == "_":
            noprefix = "__"
        else:
            noprefix = None
        while True:
            matched = sorted(w for w in words
                             if isinstance(w, str) and w.startswith(attr)
                             and w.isidentifier()
                             and not (noprefix and w.startswith(noprefix)))
            if matched or not noprefix:
                break
            noprefix = "__" if noprefix == "_" else None
        names = [f"{expr}.{w}" for w in matched]
        if len(matched) > 1 and len(commonprefix(names)) > len(text):
            return [commonprefix(names)]
        if len(matched) == 1 or self.use_colors:
            values = [_static_lookup(obj, w) for w in matched]
        else:
            values = [_MISSING] * len(matched)
        return self._finish(text, names, values)
