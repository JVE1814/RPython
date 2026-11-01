"""Read access to the symbol tables the compiler builds for source code.

``symtable(code, filename, compile_type)`` returns a `SymbolTable` for the
module; nested scopes (classes, functions, lambdas, generator expressions,
annotation and type-parameter scopes) are reachable via ``get_children()``
and each name is described by a `Symbol`.
"""

import _symtable
from _symtable import (
    USE, DEF_GLOBAL, DEF_NONLOCAL, DEF_LOCAL, DEF_PARAM, DEF_TYPE_PARAM,
    DEF_FREE_CLASS, DEF_IMPORT, DEF_BOUND, DEF_ANNOT, DEF_COMP_ITER,
    DEF_COMP_CELL, SCOPE_OFF, SCOPE_MASK,
    FREE, LOCAL, GLOBAL_IMPLICIT, GLOBAL_EXPLICIT, CELL,
)
from enum import StrEnum

__all__ = ["symtable", "SymbolTableType", "SymbolTable", "Class",
           "Function", "Symbol"]


class SymbolTableType(StrEnum):
    MODULE = "module"
    FUNCTION = "function"
    CLASS = "class"
    ANNOTATION = "annotation"
    TYPE_ALIAS = "type alias"
    TYPE_PARAMETERS = "type parameters"
    TYPE_VARIABLE = "type variable"


_KIND_BY_CODE = {
    _symtable.TYPE_MODULE: SymbolTableType.MODULE,
    _symtable.TYPE_FUNCTION: SymbolTableType.FUNCTION,
    _symtable.TYPE_CLASS: SymbolTableType.CLASS,
    _symtable.TYPE_ANNOTATION: SymbolTableType.ANNOTATION,
    _symtable.TYPE_TYPE_ALIAS: SymbolTableType.TYPE_ALIAS,
    _symtable.TYPE_TYPE_PARAMETERS: SymbolTableType.TYPE_PARAMETERS,
    _symtable.TYPE_TYPE_VARIABLE: SymbolTableType.TYPE_VARIABLE,
}

# Flag names in the order they are listed by Symbol.__repr__ and main().
_FLAG_NAMES = (
    ("USE", USE), ("DEF_GLOBAL", DEF_GLOBAL), ("DEF_NONLOCAL", DEF_NONLOCAL),
    ("DEF_LOCAL", DEF_LOCAL), ("DEF_PARAM", DEF_PARAM),
    ("DEF_TYPE_PARAM", DEF_TYPE_PARAM), ("DEF_FREE_CLASS", DEF_FREE_CLASS),
    ("DEF_IMPORT", DEF_IMPORT), ("DEF_ANNOT", DEF_ANNOT),
    ("DEF_COMP_ITER", DEF_COMP_ITER), ("DEF_COMP_CELL", DEF_COMP_CELL),
)
_SCOPE_NAMES = {LOCAL: "LOCAL", GLOBAL_EXPLICIT: "GLOBAL_EXPLICIT",
                GLOBAL_IMPLICIT: "GLOBAL_IMPLICIT", FREE: "FREE", CELL: "CELL"}


def symtable(code, filename, compile_type, *, module=None):
    """Symbol table of the module compiled from *code* (str or bytes).

    *compile_type* is the compile() mode: 'exec', 'eval' or 'single'.
    """
    entry = _symtable.symtable(code, filename, compile_type, module=module)
    return _Tables(filename).table(entry)


class _Tables:
    """Wraps raw entries; the same entry always gives the same object."""

    def __init__(self, filename):
        self.filename = filename
        self.seen = {}

    def table(self, entry):
        obj = self.seen.get(entry.id)
        if obj is None:
            kind = entry.type
            if kind == _symtable.TYPE_FUNCTION:
                cls = Function
            elif kind == _symtable.TYPE_CLASS:
                cls = Class
            else:
                cls = SymbolTable
            obj = cls(entry, self.filename)
            obj._tables = self
            self.seen[entry.id] = obj
        return obj


class SymbolTable:
    """One scope of the program."""

    def __init__(self, raw_table, filename):
        self._table = raw_table
        self._filename = filename
        self._tables = _Tables(filename)
        self._symbols = {}

    def __repr__(self):
        label = "" if type(self) is SymbolTable else type(self).__name__ + " "
        if self._table.name == "top":
            return f"<{label}SymbolTable for module {self._filename}>"
        return f"<{label}SymbolTable for {self._table.name} in {self._filename}>"

    def get_type(self):
        """Kind of scope, a `SymbolTableType` member."""
        kind = _KIND_BY_CODE.get(self._table.type)
        if kind is None:
            raise AssertionError(f"unexpected type: {self._table.type}")
        return kind

    def get_id(self):
        """Identifier of the scope (unique within one symtable() call)."""
        return self._table.id

    def get_name(self):
        """Name of the class/function, 'top' for the module."""
        return self._table.name

    def get_lineno(self):
        """First line of the block (0 for the module)."""
        return self._table.lineno

    def is_optimized(self):
        """True for function scopes, whose locals live in fast slots."""
        return self._table.type == _symtable.TYPE_FUNCTION

    def is_nested(self):
        """True if the block is nested in a function-like scope."""
        return bool(self._table.nested)

    def has_children(self):
        return bool(self._table.children)

    def get_identifiers(self):
        """Names used in this scope."""
        return self._table.symbols.keys()

    def lookup(self, name):
        """The `Symbol` for *name*; KeyError if the scope does not use it."""
        sym = self._symbols.get(name)
        if sym is None:
            flags = self._table.symbols[name]
            spaces = [t for t in self.get_children() if t.get_name() == name]
            module = self._table.type == _symtable.TYPE_MODULE
            sym = Symbol(name, flags, spaces, module_scope=module)
            self._symbols[name] = sym
        return sym

    def get_symbols(self):
        return [self.lookup(name) for name in self.get_identifiers()]

    def get_children(self):
        """Tables of the blocks nested directly in this one."""
        return [self._tables.table(e) for e in self._table.children]

    def _names_where(self, test):
        return tuple(name for name, flags in self._table.symbols.items()
                     if test(flags, (flags >> SCOPE_OFF) & SCOPE_MASK))


class Function(SymbolTable):
    """Symbol table of a function (or lambda / generator expression)."""

    def get_parameters(self):
        return self._names_where(lambda f, s: f & DEF_PARAM)

    def get_locals(self):
        return self._names_where(lambda f, s: s in (LOCAL, CELL))

    def get_globals(self):
        return self._names_where(lambda f, s: s in (GLOBAL_IMPLICIT, GLOBAL_EXPLICIT))

    def get_nonlocals(self):
        return self._names_where(lambda f, s: f & DEF_NONLOCAL)

    def get_frees(self):
        return self._names_where(lambda f, s: s == FREE)

    def get_cells(self):
        return self._names_where(lambda f, s: s == CELL)


class Class(SymbolTable):
    """Symbol table of a class body."""

    def get_methods(self):
        """Names of the functions defined directly in the class body.

        Deprecated (removal planned for 3.16).
        """
        import warnings
        warnings.warn('symtable.Class.get_methods() is deprecated '
                      'and will be removed in Python 3.16.',
                      DeprecationWarning, stacklevel=2)
        result = []
        for child in self._table.children:
            # `def m[T]()` puts the function inside a type-parameter block
            if child.type == _symtable.TYPE_TYPE_PARAMETERS:
                inner = [c for c in child.children
                         if c.type == _symtable.TYPE_FUNCTION and c.name == child.name]
                if not inner:
                    continue
                child = inner[0]
            if child.type != _symtable.TYPE_FUNCTION or child.name.startswith('<'):
                continue
            flags = self._table.symbols.get(child.name, 0)
            if (flags >> SCOPE_OFF) & SCOPE_MASK == GLOBAL_EXPLICIT:
                continue
            if child.name not in result:
                result.append(child.name)
        return tuple(result)


class Symbol:
    """Information about one name in one scope."""

    def __init__(self, name, flags, namespaces=None, *, module_scope=False):
        self.__name = name
        self.__flags = flags
        self.__scope = (flags >> SCOPE_OFF) & SCOPE_MASK
        self.__namespaces = namespaces or ()
        self.__module_scope = module_scope

    def __repr__(self):
        names = [n for n, bit in _FLAG_NAMES if self.__flags & bit]
        scope = _SCOPE_NAMES.get(self.__scope, "UNKNOWN")
        return f"<symbol {self.__name!r}: {scope}, {'|'.join(names)}>"

    def get_name(self):
        return self.__name

    def is_referenced(self):
        """The name is read in this scope."""
        return bool(self.__flags & USE)

    def is_parameter(self):
        return bool(self.__flags & DEF_PARAM)

    def is_type_parameter(self):
        return bool(self.__flags & DEF_TYPE_PARAM)

    def is_global(self):
        """A global name (module-level bindings count as global)."""
        if self.__scope in (GLOBAL_IMPLICIT, GLOBAL_EXPLICIT):
            return True
        return bool(self.__module_scope and self.__flags & DEF_BOUND)

    def is_declared_global(self):
        """Named in a ``global`` statement."""
        return self.__scope == GLOBAL_EXPLICIT

    def is_local(self):
        """Bound in this scope (module-level bindings count as local)."""
        if self.__scope in (LOCAL, CELL):
            return True
        return bool(self.__module_scope and self.__flags & DEF_BOUND)

    def is_annotated(self):
        return bool(self.__flags & DEF_ANNOT)

    def is_free(self):
        """Refers to a variable of an enclosing function."""
        return self.__scope == FREE

    def is_free_class(self):
        return bool(self.__flags & DEF_FREE_CLASS)

    def is_cell(self):
        """Local variable that nested functions refer to."""
        return self.__scope == CELL

    def is_imported(self):
        return bool(self.__flags & DEF_IMPORT)

    def is_assigned(self):
        return bool(self.__flags & DEF_LOCAL)

    def is_nonlocal(self):
        return bool(self.__flags & DEF_NONLOCAL)

    def is_comp_iter(self):
        return bool(self.__flags & DEF_COMP_ITER)

    def is_comp_cell(self):
        return bool(self.__flags & DEF_COMP_CELL)

    def is_namespace(self):
        """The name is bound to a class or function defined here."""
        return bool(self.__namespaces)

    def get_namespaces(self):
        return self.__namespaces

    def get_namespace(self):
        """The single namespace bound to the name (ValueError otherwise)."""
        if len(self.__namespaces) == 0:
            raise ValueError("name is not bound to any namespaces")
        if len(self.__namespaces) > 1:
            raise ValueError("name is bound to multiple namespaces")
        return self.__namespaces[0]


def _describe(table, depth, out):
    pad = "    " * depth
    if table.get_type() == SymbolTableType.MODULE:
        out.append(f"{pad}symbol table for module from file {table._filename!r}:")
    else:
        out.append(f"{pad}symbol table for {table.get_type()} {table.get_name()!r}:")
    for sym in table.get_symbols():
        flags = sym._Symbol__flags
        scope = _SCOPE_NAMES.get(sym._Symbol__scope, "unknown").lower()
        names = ", ".join(n.lower() for n, bit in _FLAG_NAMES if flags & bit)
        out.append(f"{pad}    {scope} symbol {sym.get_name()!r}: {names}")
    for child in table.get_children():
        out.append("")
        _describe(child, depth + 1, out)


def main(args=None):
    """Print the symbol tables of a file (or stdin, '-')."""
    import sys
    if args is None:
        args = sys.argv[1:]
    paths = list(args) or ['-']
    for path in paths:
        if path == '-':
            source = sys.stdin.read()
            filename = '<stdin>'
        else:
            with open(path, 'rb') as f:
                source = f.read()
            filename = path
        out = []
        _describe(symtable(source, filename, 'exec'), 0, out)
        print("\n".join(out))


if __name__ == "__main__":
    main()
