"""Completion of module names (and imported names) in import statements.

``ImportParser`` looks at the last statement of the text before the
cursor and, if it is an unfinished ``import``/``from ... import``, says
which dotted name is being typed.  ``ModuleCompleter`` lists candidate
modules from sys.path (top-level names, submodules of packages, a few
well-known pseudo-submodules) and, for ``from X import ...``, the public
attributes of X.  Standard library modules without import side effects are
imported automatically; other modules are only imported on request (the
reader offers an "action" for that).
"""

from __future__ import annotations

import importlib
import importlib.machinery
import importlib.util
import io
import keyword
import os
import pkgutil
import sys
import tokenize
import types
from pkgutil import ModuleInfo

__all__ = ["ModuleCompleter", "ImportParser", "HARDCODED_SUBMODULES"]

# Names importable as "parent.name" although they are not found as files
# inside a package directory.
HARDCODED_SUBMODULES = {
    "collections": ["abc"],
    "math": ["integer"],
    "os": ["path"],
    "xml.parsers.expat": ["errors", "model"],
}

# Standard library modules that must never be imported just to complete
# names (they print, open browsers/windows or start programs).
_UNSAFE_TO_AUTOIMPORT = {
    "antigravity", "this", "idlelib", "turtledemo", "turtle", "tkinter",
    "pydoc_data", "test", "ensurepip", "venv", "webbrowser", "_pyrepl",
    "__hello__", "__phello__", "lib2to3", "xxsubtype",
}

IMPORT_PROMPT = ("[ module not imported, press again to import it "
                 "and propose attributes ]")

_MODULE_MARKER = types.ModuleType("<module>")


def _make_import_error_message(exc: BaseException) -> str:
    return f"[ error during import: {exc} ]"


class ModuleCompleter:
    """Candidates for the module/attribute name being typed in an import."""

    def __init__(self, namespace: dict | None = None) -> None:
        self.namespace = namespace or {}
        self._global_cache: list[ModuleInfo] = []
        self._cache_key: tuple | None = None
        self._failed_imports: set[str] = set()
        self._stdlib_path = os.path.dirname(importlib.__path__[0])

    # -- entry point ---------------------------------------------------------------

    def get_completions(self, line: str):
        """None if *line* does not end in an unfinished import; otherwise
        (names, values, action) where *action* is None or (message,
        function); calling the function imports the module and returns
        an error message or None."""
        parsed = ImportParser(line).parse()
        if parsed is None:
            return None
        from_name, name = parsed
        try:
            return self.complete(from_name, name)
        except Exception:
            return [], [], None

    def complete(self, from_name: str | None, name: str | None):
        if from_name is None:
            names, values = self.find_modules(name)
            return names, values, None
        if name is None:
            names, values = self.find_modules(from_name)
            return names, values, None
        return self.find_names_in_module(from_name, name)

    # -- helpers on dotted names ---------------------------------------------------

    def get_path_and_prefix(self, dotted_name: str) -> tuple[str, str]:
        """Split 'a.b.c' into ('a.b', 'c'); leading dots belong to the
        path ('.foo' -> ('.', 'foo'))."""
        if not dotted_name.strip("."):
            return dotted_name, ""
        if "." not in dotted_name:
            return "", dotted_name
        head, _, tail = dotted_name.rpartition(".")
        if not head.strip("."):
            head += "."
        return head, tail

    def _resolve(self, name: str) -> str | None:
        """Absolute module name for a possibly relative *name*."""
        if not name.startswith("."):
            return name
        package = self.namespace.get("__package__")
        if not package:
            return None
        try:
            return importlib.util.resolve_name(name, package)
        except (ImportError, ValueError):
            return None

    @staticmethod
    def _join(path: str, name: str) -> str:
        return path + name if path.endswith(".") or not path else f"{path}.{name}"

    @staticmethod
    def _wanted(name: str, prefix: str) -> bool:
        if not name.startswith(prefix) or not name.isidentifier():
            return False
        if keyword.iskeyword(name):
            return False
        return prefix.startswith("_") or not name.startswith("_")

    # -- module listing ---------------------------------------------------------------

    def global_cache(self) -> list[ModuleInfo]:
        key = tuple(sys.path)
        if self._cache_key != key:
            self._global_cache = list(pkgutil.iter_modules())
            self._cache_key = key
        return self._global_cache

    def find_modules(self, dotted: str):
        path, prefix = self.get_path_and_prefix(dotted)
        if not path:
            found = {info.name for info in self.global_cache()}
            found.update(sys.builtin_module_names)
            names = sorted(n for n in found if self._wanted(n, prefix))
            return names, [_MODULE_MARKER] * len(names)
        parent = self._resolve(path)
        if parent is None:
            return [], []
        subs = self._submodule_names(parent)
        names = sorted(n for n in subs if self._wanted(n, prefix))
        return [self._join(path, n) for n in names], [_MODULE_MARKER] * len(names)

    def _submodule_names(self, parent: str) -> set[str]:
        names = set()
        try:
            for info in self.iter_submodules(parent):
                names.add(info.name)
        except Exception:
            pass
        if parent in HARDCODED_SUBMODULES and self._is_stdlib(parent):
            names.update(HARDCODED_SUBMODULES[parent])
        return names

    def iter_submodules(self, parent: str):
        """ModuleInfo for each submodule of package *parent*."""
        locations = self._package_locations(parent)
        if not locations:
            return []
        return list(pkgutil.iter_modules(locations))

    def _package_locations(self, name: str) -> list[str] | None:
        module = sys.modules.get(name)
        if module is not None:
            spec = getattr(module, "__spec__", None)
            if spec is None or spec.origin is None and not spec.submodule_search_locations:
                return None
            if spec.origin is None or not getattr(spec, "has_location", True):
                return None
            locations = spec.submodule_search_locations
            return list(locations) if locations is not None else None
        head, _, tail = name.rpartition(".")
        if head:
            search = self._package_locations(head)
            if not search:
                return None
        else:
            search = list(sys.path)
        for entry in search:
            importer = pkgutil.get_importer(entry) if isinstance(entry, str) else None
            if importer is None or not hasattr(importer, "find_spec"):
                continue
            try:
                spec = importer.find_spec(name)
            except Exception:
                continue
            if spec is None:
                continue
            if spec.submodule_search_locations is None:
                return None
            return list(spec.submodule_search_locations)
        return None

    def _module_origin(self, name: str) -> str | None:
        module = sys.modules.get(name)
        if module is not None:
            spec = getattr(module, "__spec__", None)
            origin = getattr(spec, "origin", None) if spec is not None else None
            if origin in ("built-in", "frozen"):
                return origin
            return getattr(module, "__file__", None) or origin
        top = name.split(".")[0]
        if top in sys.builtin_module_names:
            return "built-in"
        head, _, _ = name.rpartition(".")
        if head:
            locations = self._package_locations(head)
            search = locations or []
        else:
            search = list(sys.path)
        tail = name.rpartition(".")[2]
        for entry in search:
            importer = pkgutil.get_importer(entry) if isinstance(entry, str) else None
            if importer is None or not hasattr(importer, "find_spec"):
                continue
            try:
                spec = importer.find_spec(name)
            except Exception:
                continue
            if spec is not None:
                return spec.origin or (list(spec.submodule_search_locations or [""])[0] + os.sep + tail)
        try:
            spec = importlib.machinery.FrozenImporter.find_spec(name)
        except Exception:
            spec = None
        if spec is not None:
            return "frozen"
        return None

    def _is_stdlib(self, name: str) -> bool:
        top = name.split(".")[0]
        origin = self._module_origin(top)
        if origin in ("built-in", "frozen"):
            return True
        if origin is None:
            return False
        stdlib = os.path.normcase(os.path.abspath(self._stdlib_path))
        path = os.path.normcase(os.path.abspath(origin))
        if not path.startswith(stdlib + os.sep):
            return False
        rest = path[len(stdlib) + 1:]
        return not rest.startswith(("site-packages", "dist-packages"))

    # -- names from a module -------------------------------------------------------------

    def _safe_to_autoimport(self, name: str) -> bool:
        parts = name.split(".")
        if "__main__" in parts or parts[0] in _UNSAFE_TO_AUTOIMPORT:
            return False
        if name in _UNSAFE_TO_AUTOIMPORT:
            return False
        return self._is_stdlib(name)

    def _maybe_import_module(self, name: str) -> types.ModuleType | None:
        """Import *name* if it is a standard library module known to be
        free of import side effects."""
        if name in sys.modules:
            return sys.modules[name]
        if name in self._failed_imports or not self._safe_to_autoimport(name):
            return None
        return self._import(name)[0]

    def _import(self, name: str):
        before = set(sys.modules)
        try:
            module = importlib.import_module(name)
        except (Exception, SystemExit) as exc:
            self._failed_imports.add(name)
            for added in set(sys.modules) - before:
                sys.modules.pop(added, None)
            return None, _make_import_error_message(exc)
        return module, None

    def _attributes(self, module) -> dict:
        try:
            names = dir(module)
        except Exception:
            return {}
        result = {}
        namespace = getattr(module, "__dict__", {})
        for n in names:
            if not isinstance(n, str):
                continue
            if n in namespace:
                result[n] = namespace[n]
            else:
                try:
                    result[n] = getattr(module, n)
                except Exception:
                    result[n] = None
        return result

    def find_names_in_module(self, from_name: str, prefix: str):
        module_name = self._resolve(from_name)
        if module_name is None:
            return [], [], None
        candidates: dict[str, object] = {}
        for sub in self._submodule_names(module_name):
            if self._wanted(sub, prefix):
                candidates[sub] = _MODULE_MARKER
        action = None
        module = sys.modules.get(module_name)
        if module is None:
            module = self._maybe_import_module(module_name)
        if module is not None:
            for attr, value in self._attributes(module).items():
                if self._wanted(attr, prefix):
                    candidates[attr] = value
        elif module_name not in self._failed_imports and self._may_offer_import(module_name):
            def do_import(name=module_name):
                _module, error = self._import(name)
                return error
            action = (IMPORT_PROMPT, do_import)
        names = sorted(candidates)
        return names, [candidates[n] for n in names], action

    def _may_offer_import(self, name: str) -> bool:
        parts = name.split(".")
        if "__main__" in parts:
            return False
        if self._is_stdlib(name):
            # standard modules are imported automatically when safe;
            # the unsafe ones are never imported for completion
            return False
        return True


# --- parsing the import statement being typed -------------------------------------------------

class ImportParser:
    """Find the dotted name being typed at the end of an import statement.

    ``parse()`` returns (from_name, name): ``(None, 'a.b')`` while typing
    ``import a.b``, ``('a.b', None)`` for ``from a.b`` and ``('a', 'x')``
    for ``from a import x``; None when the text does not end inside an
    import statement.
    """

    _SKIP = {tokenize.NL, tokenize.COMMENT, tokenize.INDENT, tokenize.DEDENT,
             tokenize.ENDMARKER}

    def __init__(self, code: str) -> None:
        self.code = code

    def _statement(self) -> list | None:
        tokens = []
        try:
            for tok in tokenize.generate_tokens(io.StringIO(self.code).readline):
                tokens.append(tok)
        except (tokenize.TokenError, SyntaxError) as exc:
            msg = str(exc.args[0]) if exc.args else ""
            if isinstance(exc, SyntaxError) or "EOF" not in msg:
                return None
        statement = []
        for tok in tokens:
            if tok.type in self._SKIP:
                continue
            if tok.type == tokenize.NEWLINE:
                if tok.string:
                    statement = []
                continue
            if tok.type == tokenize.OP and tok.string == ";":
                statement = []
                continue
            statement.append(tok)
        return statement

    def parse(self) -> tuple[str | None, str | None] | None:
        if not self.code or self.code.endswith("\n"):
            return None
        statement = self._statement()
        if not statement:
            return None
        trailing_space = self.code[-1] in " \t"
        first = statement[0]
        if first.type != tokenize.NAME:
            return None
        rest = statement[1:]
        if first.string == "import":
            return self._parse_import(rest, trailing_space)
        if first.string == "from":
            return self._parse_from(rest, trailing_space)
        return None

    @staticmethod
    def _is_name(tok) -> bool:
        return tok.type == tokenize.NAME and not keyword.iskeyword(tok.string)

    def _parse_import(self, toks, trailing_space):
        current = ""
        state = "expect_name"
        for tok in toks:
            s = tok.string
            if self._is_name(tok):
                if state in ("expect_name", "after_dot"):
                    current += s
                    state = "in_name"
                elif state == "after_as":
                    state = "after_alias"
                else:
                    return None
            elif tok.type == tokenize.NAME and s == "as":
                if state != "in_name":
                    return None
                state = "after_as"
            elif tok.type == tokenize.OP and s == ".":
                if state != "in_name":
                    return None
                current += "."
                state = "after_dot"
            elif tok.type == tokenize.OP and s == ",":
                if state not in ("in_name", "after_alias"):
                    return None
                current = ""
                state = "expect_name"
            else:
                return None
        if state in ("after_as", "after_alias"):
            return None
        if trailing_space and state in ("in_name", "after_dot"):
            return None
        return None, current

    def _parse_from(self, toks, trailing_space):
        module = ""
        state = "start"
        index = 0
        while index < len(toks):
            tok = toks[index]
            s = tok.string
            if tok.type == tokenize.OP and s in (".", "..."):
                if state in ("start", "lead_dots"):
                    module += s
                    state = "lead_dots"
                elif state == "in_name":
                    module += "."
                    state = "after_dot"
                else:
                    return None
            elif self._is_name(tok):
                if state in ("start", "lead_dots", "after_dot"):
                    module += s
                    state = "in_name"
                else:
                    return None
            elif tok.type == tokenize.NAME and s == "import":
                if state not in ("in_name", "lead_dots"):
                    return None
                return self._parse_from_names(module, toks[index + 1:], trailing_space)
            else:
                return None
            index += 1
        if trailing_space and state in ("in_name", "after_dot"):
            return None
        return module, None

    def _parse_from_names(self, module, toks, trailing_space):
        current = ""
        state = "expect_name"
        for i, tok in enumerate(toks):
            s = tok.string
            if tok.type == tokenize.OP and s == "(" and i == 0:
                continue
            if self._is_name(tok):
                if state == "expect_name":
                    current = s
                    state = "in_name"
                elif state == "after_as":
                    state = "after_alias"
                else:
                    return None
            elif tok.type == tokenize.NAME and s == "as":
                if state != "in_name":
                    return None
                state = "after_as"
            elif tok.type == tokenize.OP and s == ",":
                if state not in ("in_name", "after_alias"):
                    return None
                current = ""
                state = "expect_name"
            else:
                return None
        if state in ("after_as", "after_alias"):
            return None
        if trailing_space and state == "in_name":
            return None
        return module, current
