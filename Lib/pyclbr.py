"""Class and function browser support: describe a module's top-level
classes and functions by reading its source (without importing it).

readmodule_ex(module, path=None) returns a dict mapping names to Class and
Function objects (plus '__path__' for packages); readmodule() keeps only
the classes.  Each object records its module, name, file, lineno,
end_lineno, parent and nested children; classes also record their base
classes (`super`, resolved to Class objects where possible) and their
methods (name -> line number).
"""

import ast
import importlib.machinery
import sys

__all__ = ["readmodule", "readmodule_ex", "Class", "Function"]

# fully qualified module name -> its dict
_modules = {}


class _Object:
    """Common data of classes and functions."""

    def __init__(self, module, name, file, lineno, end_lineno, parent):
        self.module = module
        self.name = name
        self.file = file
        self.lineno = lineno
        self.end_lineno = end_lineno
        self.parent = parent
        self.children = {}
        if parent is not None:
            parent._add_child(name, self)

    def _add_child(self, name, obj):
        self.children[name] = obj


class Function(_Object):
    """A function (or method) definition."""

    def __init__(self, module, name, file, lineno, parent=None, is_async=False,
                 *, end_lineno=None):
        super().__init__(module, name, file, lineno, end_lineno, parent)
        self.is_async = is_async
        if isinstance(parent, Class):
            parent.methods[name] = lineno


class Class(_Object):
    """A class definition."""

    def __init__(self, module, name, super_, file, lineno, parent=None, *,
                 end_lineno=None):
        self.methods = {}
        super().__init__(module, name, file, lineno, end_lineno, parent)
        self.super = super_ or []


def _nest_function(ob, func_name, lineno, end_lineno, is_async=False):
    """A Function nested in `ob` (a function or class)."""
    return Function(ob.module, func_name, ob.file, lineno,
                    parent=ob, is_async=is_async, end_lineno=end_lineno)


def _nest_class(ob, class_name, lineno, end_lineno, super=None):
    """A Class nested in `ob`."""
    return Class(ob.module, class_name, super, ob.file, lineno,
                 parent=ob, end_lineno=end_lineno)


def readmodule(module, path=None):
    """Only the classes of readmodule_ex(module, path)."""
    return {name: value for name, value in _readmodule(module, path or []).items()
            if isinstance(value, Class)}


def readmodule_ex(module, path=None):
    """Classes and functions defined in (or imported into) `module`,
    searched on `path` + sys.path."""
    return _readmodule(module, path or [])


def _find_spec(fullname, search_path):
    loaded = sys.modules.get(fullname)
    if loaded is not None:
        spec = getattr(loaded, '__spec__', None)
        if spec is not None:
            return spec
    return importlib.machinery.PathFinder.find_spec(fullname, search_path)


def _readmodule(module, path, inpackage=None):
    fullname = f"{inpackage}.{module}" if inpackage else module
    cached = _modules.get(fullname)
    if cached is not None:
        return cached

    if inpackage is None and module in sys.builtin_module_names:
        # nothing to read
        result = _modules[module] = {}
        return result

    package, dot, last = module.rpartition('.')
    if dot:
        parent = _readmodule(package, path, inpackage)
        qualified_package = f"{inpackage}.{package}" if inpackage else package
        if '__path__' not in parent:
            raise ImportError(f'No package named {qualified_package}')
        return _readmodule(last, parent['__path__'], qualified_package)

    search_path = list(path) if inpackage is not None else list(path) + sys.path
    spec = _find_spec(fullname, search_path)
    if spec is None:
        raise ModuleNotFoundError(f"no module named {fullname!r}", name=fullname)

    result = _modules[fullname] = {}
    if spec.submodule_search_locations is not None:
        result['__path__'] = spec.submodule_search_locations
    loader = spec.loader
    try:
        source = loader.get_source(fullname)
    except (AttributeError, ImportError):
        return result
    if source is None:
        return result
    try:
        filename = loader.get_filename(fullname)
    except (AttributeError, ImportError):
        filename = spec.origin
    return _create_tree(fullname, path, filename, source, result, inpackage)


class _TreeBuilder(ast.NodeVisitor):
    """Walks a module's AST and fills the result dict."""

    def __init__(self, module, path, file, tree, inpackage):
        self.module = module
        self.path = path
        self.file = file
        self.tree = tree
        self.inpackage = inpackage
        self.scopes = []            # enclosing Class/Function objects
        self.module_aliases = {}    # local name -> imported module name

    # -- definitions

    def _resolve_base(self, node):
        if isinstance(node, ast.Name):
            found = self.tree.get(node.id)
            return found if found is not None else node.id
        if isinstance(node, ast.Attribute):
            dotted = ast.unparse(node)
            owner, _, attr = dotted.rpartition('.')
            target = self.module_aliases.get(owner, owner)
            known = _modules.get(target)
            if known is not None and isinstance(known.get(attr), Class):
                return known[attr]
            return dotted
        return ast.unparse(node)

    def visit_ClassDef(self, node):
        bases = [self._resolve_base(base) for base in node.bases]
        if self.scopes:
            obj = _nest_class(self.scopes[-1], node.name, node.lineno,
                              node.end_lineno, bases)
        else:
            obj = Class(self.module, node.name, bases, self.file, node.lineno,
                        end_lineno=node.end_lineno)
            self.tree[node.name] = obj
        self.scopes.append(obj)
        self.generic_visit(node)
        self.scopes.pop()

    def _function(self, node, is_async):
        if self.scopes:
            obj = _nest_function(self.scopes[-1], node.name, node.lineno,
                                 node.end_lineno, is_async)
        else:
            obj = Function(self.module, node.name, self.file, node.lineno,
                           is_async=is_async, end_lineno=node.end_lineno)
            self.tree[node.name] = obj
        self.scopes.append(obj)
        self.generic_visit(node)
        self.scopes.pop()

    def visit_FunctionDef(self, node):
        self._function(node, False)

    def visit_AsyncFunctionDef(self, node):
        self._function(node, True)

    # -- imports at module level make other modules' objects visible

    def _read_other(self, name):
        try:
            return _readmodule(name, self.path, self.inpackage)
        except Exception:
            try:
                return _readmodule(name, [], None)
            except Exception:
                return None

    def visit_Import(self, node):
        if self.scopes:
            return
        for alias in node.names:
            if self._read_other(alias.name) is not None:
                local = alias.asname or alias.name
                self.module_aliases[local] = alias.name

    def visit_ImportFrom(self, node):
        if self.scopes:
            return
        source = node.module or ''
        if node.level:
            package = self.module if '__path__' in self.tree else self.module.rpartition('.')[0]
            for _ in range(node.level - 1):
                package = package.rpartition('.')[0]
            if not package:
                return
            source = f"{package}.{source}" if source else package
        names = self._read_other(source)
        if names is None:
            return
        for alias in node.names:
            if alias.name == '*':
                for key, value in names.items():
                    if not key.startswith('_'):
                        self.tree[key] = value
            elif alias.name in names:
                self.tree[alias.asname or alias.name] = names[alias.name]


def _create_tree(fullmodule, path, fname, source, tree, inpackage):
    """Fill `tree` with the classes and functions of `source` and return it."""
    builder = _TreeBuilder(fullmodule, path, fname, tree, inpackage)
    builder.visit(ast.parse(source))
    return tree


def _main():
    """Print the outline of the module named on the command line."""
    import os
    try:
        target = sys.argv[1]
    except IndexError:
        target = __file__
    if os.path.exists(target):
        directory, filename = os.path.split(target)
        target = os.path.splitext(filename)[0]
        path = [directory]
    else:
        path = []
    tree = readmodule_ex(target, path)
    entries = [value for value in tree.values() if isinstance(value, _Object)]
    entries.sort(key=lambda obj: getattr(obj, 'lineno', 0))
    pending = [(obj, 0) for obj in reversed(entries)]
    while pending:
        obj, depth = pending.pop()
        indent = "  " * depth
        if isinstance(obj, Class):
            print(f"{indent}class {obj.name} {obj.super} {obj.lineno}")
        else:
            print(f"{indent}def {obj.name} {obj.lineno}")
        children = sorted(obj.children.values(), key=lambda c: c.lineno,
                          reverse=True)
        pending.extend((child, depth + 1) for child in children)


if __name__ == "__main__":
    _main()
