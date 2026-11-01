"""Tab completion of Python identifiers, keywords and attributes.

``Completer(namespace).complete(text, state)`` follows the readline completer
protocol: it is called with state 0, 1, 2, ... and returns the successive
candidates for *text*, then None.  Plain names are looked up among the
keywords, the namespace (by default ``__main__.__dict__``) and the builtins;
dotted expressions ``a.b.c`` are evaluated up to the last dot and completed
from ``dir()`` of the result.  Callables get a trailing ``(`` (or ``()`` when
they take no arguments).

Importing this module installs a default completer in the readline module,
when readline is available.
"""

import atexit
import builtins
import inspect
import keyword
import re
import types

import __main__

__all__ = ["Completer"]

# Keywords that are complete statements/values by themselves get no suffix;
# block openers that take no expression get a colon; the rest a space.
_KW_NO_SUFFIX = frozenset({"False", "None", "True", "break", "continue",
                           "pass", "else", "_"})
_KW_COLON = frozenset({"finally", "try"})

_ATTR_EXPR = re.compile(r"(\w+(?:\.\w+)*)\.(\w*)")

# Class attributes of these kinds are completed without calling them on the
# instance (their getters may be expensive, have side effects or fail).
_LAZY_DESCRIPTORS = (property, types.MemberDescriptorType,
                     types.GetSetDescriptorType)


def _keyword_candidate(word):
    if word in _KW_COLON:
        return word + ":"
    if word in _KW_NO_SUFFIX:
        return word
    return word + " "


def _is_lazy(cls_attr):
    if isinstance(cls_attr, _LAZY_DESCRIPTORS):
        return True
    # Any other non-callable descriptor (custom __get__).
    return (hasattr(type(cls_attr), "__get__") and not callable(cls_attr)
            and not isinstance(cls_attr, (classmethod, staticmethod)))


class Completer:

    def __init__(self, namespace=None):
        """Complete names from *namespace* (a dict), or from ``__main__``'s
        globals at each completion when no namespace is given."""
        if namespace is not None and not isinstance(namespace, dict):
            raise TypeError("namespace must be a dictionary")
        self.use_main_ns = namespace is None
        if namespace is not None:
            self.namespace = namespace

    def complete(self, text, state):
        """Return the candidate number *state* for *text*, or None."""
        if self.use_main_ns:
            self.namespace = __main__.__dict__

        if not text.strip():
            # Nothing typed yet: the Tab key should indent.
            if state != 0:
                return None
            if _readline_available:
                readline.insert_text("\t")
                readline.redisplay()
                return ""
            return "\t"

        if state == 0:
            if "." in text:
                self.matches = self.attr_matches(text)
            else:
                self.matches = self.global_matches(text)
        if state < len(self.matches):
            return self.matches[state]
        return None

    def _callable_postfix(self, val, word):
        if not callable(val):
            return word
        word += "("
        try:
            if not inspect.signature(val).parameters:
                word += ")"
        except (ValueError, TypeError):
            pass
        return word

    def global_matches(self, text):
        """Candidates among keywords, the namespace and the builtins."""
        found = []
        seen = {"__builtins__"}
        for word in keyword.kwlist + keyword.softkwlist:
            if word.startswith(text) and word not in seen:
                seen.add(word)
                found.append(_keyword_candidate(word))
        for names in (self.namespace, builtins.__dict__):
            for word, value in names.items():
                if word.startswith(text) and word not in seen:
                    seen.add(word)
                    found.append(self._callable_postfix(value, word))
        return found

    def attr_matches(self, text):
        """Candidates for a dotted expression ``expr.partial``.

        Only *expr* is evaluated (in the completer's namespace); attributes
        whose names start with '_' are offered only when *partial* starts with
        '_' (or nothing else matches), dunder names only for '__'.
        """
        m = _ATTR_EXPR.fullmatch(text)
        if m is None:
            return []
        expr, attr = m.group(1), m.group(2)
        try:
            obj = eval(expr, self.namespace)
        except Exception:
            return []

        names = set(dir(obj))
        names.discard("__builtins__")
        if hasattr(obj, "__class__"):
            names.add("__class__")
            names.update(_class_members(obj.__class__))

        if attr == "":
            hidden = "_"
        elif attr == "_":
            hidden = "__"
        else:
            hidden = None

        n = len(attr)
        cls = type(obj)
        while True:
            found = []
            for name in names:
                if not name.startswith(attr):
                    continue
                if hidden and name.startswith(hidden):
                    continue
                full = f"{expr}.{name}"
                try:
                    cls_attr = getattr(cls, name, None)
                except Exception:
                    cls_attr = None
                if cls_attr is not None and _is_lazy(cls_attr):
                    found.append(full)
                    continue
                try:
                    value = getattr(obj, name, None)
                except Exception:
                    value = None
                if value is None:
                    found.append(full)
                else:
                    found.append(self._callable_postfix(value, full))
            if found or hidden is None:
                break
            hidden = "__" if hidden == "_" else None
        found.sort()
        return found


def _class_members(cls):
    members = dir(cls)
    for base in getattr(cls, "__bases__", ()):
        members = members + _class_members(base)
    return members


try:
    import readline
except ImportError:
    _readline_available = False
else:
    readline.set_completer(Completer().complete)
    # Drop the reference to the completer before module teardown.
    atexit.register(lambda: readline.set_completer(None))
    _readline_available = True
