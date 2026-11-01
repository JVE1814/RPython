#!/usr/bin/env python3
"""Documentation generator and online help system.

``help(obj)`` (the builtin) shows the documentation of an object; ``help()``
starts an interactive help session.  From the command line:

    python -m pydoc NAME        show the documentation of NAME (a module,
                                keyword, topic or dotted path to an object;
                                a name containing a path separator is read
                                as a source file)
    python -m pydoc -k WORD     search the one-line summaries of all modules
    python -m pydoc -w NAME     write NAME.html to the current directory
    python -m pydoc -p PORT     serve HTML documentation on localhost:PORT
    python -m pydoc -n HOST     serve on the given host name
    python -m pydoc -b          serve on a free port and open a browser

Links to the library reference point to $PYTHONDOCS (default: the online
documentation for this version).
"""

__all__ = ['help']

import __future__
import builtins
import importlib._bootstrap
import importlib._bootstrap_external
import importlib.machinery
import importlib.util
import inspect
import io
import os
import pkgutil
import platform
import re
import sys
import sysconfig
import textwrap
import time
import tokenize
import warnings
from annotationlib import Format
from collections import deque
from reprlib import Repr
from traceback import format_exception_only


# ----------------------------------------------------------------- helpers

def pathdirs():
    """Return the existing directories of sys.path (absolute, no repeats)."""
    found, seen = [], set()
    for entry in sys.path:
        path = os.path.abspath(entry or '.')
        key = os.path.normcase(path)
        if key in seen or not os.path.isdir(path):
            continue
        seen.add(key)
        found.append(path)
    return found


def replace(text, *pairs):
    """Apply the replacements old1, new1, old2, new2, ... in order."""
    for index in range(0, len(pairs) - 1, 2):
        text = text.replace(pairs[index], pairs[index + 1])
    return text


def cram(text, maxlen):
    """Shorten *text* to at most *maxlen* characters by eliding its middle."""
    if len(text) <= maxlen:
        return text
    head = max(0, (maxlen - 3) // 2)
    tail = max(0, maxlen - 3 - head)
    return text[:head] + '...' + (text[-tail:] if tail else '')


_ADDRESS = re.compile(r' at 0x[0-9a-f]{6,16}(>+)$', re.IGNORECASE)


def stripid(text):
    """Drop the ' at 0x...' address from a default object repr."""
    return _ADDRESS.sub(r'\1', text)


def plain(text):
    """Remove the backspace overstriking used for bold text."""
    return re.sub('.\b', '', text)


def _own_docstring(obj):
    """obj.__doc__ unless it merely repeats the docstring of its type."""
    try:
        text = object.__getattribute__(obj, '__doc__')
    except AttributeError:
        return None
    if not isinstance(text, str):
        return None
    if obj is not type:
        type_text = getattr(type(obj), '__doc__', None)
        if isinstance(type_text, str) and type_text == text:
            return None
    return text


def _owner_class(obj):
    """The class a method, property or descriptor was defined in, if it
    can be determined."""
    if inspect.ismethod(obj):
        bound = obj.__self__
        return bound if inspect.isclass(bound) else type(bound)
    owner = getattr(obj, '__objclass__', None)
    if inspect.isclass(owner):
        return owner
    func = obj
    if isinstance(obj, property):
        func = obj.fget
    elif isinstance(obj, (staticmethod, classmethod)):
        func = obj.__func__
    qualname = getattr(func, '__qualname__', None)
    module = sys.modules.get(getattr(func, '__module__', None) or '')
    if not isinstance(qualname, str) or module is None:
        return None
    found = module
    for part in qualname.split('.')[:-1]:
        if part == '<locals>':
            return None
        found = getattr(found, part, None)
        if found is None:
            return None
    return found if inspect.isclass(found) else None


def _inherited_docstring(obj):
    """Docstring of the same-named attribute in a base class of the class
    defining *obj* (for overriding methods and properties)."""
    name = getattr(obj, '__name__', None)
    if not isinstance(name, str) and isinstance(obj, property) and obj.fget:
        name = getattr(obj.fget, '__name__', None)
    owner = _owner_class(obj) if isinstance(name, str) else None
    if owner is None:
        return None
    for base in owner.__mro__[1:]:
        try:
            candidate = base.__dict__.get(name)
        except Exception:
            continue
        if candidate is not None:
            text = _own_docstring(candidate)
            if text is not None:
                return text
    return None


def _getdoc(obj):
    """Cleaned docstring of *obj* (own or inherited), or None."""
    text = _own_docstring(obj)
    if text is None:
        try:
            text = _inherited_docstring(obj)
        except (AttributeError, TypeError):
            text = None
    return inspect.cleandoc(text) if isinstance(text, str) else None


def getdoc(object):
    """Docstring (or preceding comments) of *object*, without leading
    blank lines; '' if there is none."""
    text = _getdoc(object) or inspect.getcomments(object)
    if not text:
        return ''
    return re.sub('^ *\n', '', text.rstrip())


def splitdoc(doc):
    """Split a docstring into (synopsis line, rest).  The synopsis is ''
    unless the first line is followed by a blank line (or is alone)."""
    lines = doc.strip().split('\n')
    if len(lines) == 1:
        return lines[0], ''
    if not lines[1].rstrip():
        return lines[0], '\n'.join(lines[2:])
    return '', '\n'.join(lines)


def classname(object, modname):
    """Name of class *object*, qualified with its module unless that is
    *modname*."""
    module = getattr(object, '__module__', None)
    if module == modname:
        return object.__name__
    return f'{module}.{object.__name__}'


def _scope_name(func, modname):
    """Where function *func* was defined ('Class', 'module.Class' or
    'module'), or None if that is the module *modname* itself."""
    qualname = getattr(func, '__qualname__', None)
    if not isinstance(qualname, str):
        return None
    module = getattr(func, '__module__', None)
    if '.' in qualname and '<locals>' not in qualname:
        scope = qualname.rpartition('.')[0]
        if module is not None and module != modname:
            return f'{module}.{scope}'
        return scope
    if module != modname:
        return module
    return None


def isdata(object):
    """True for objects documented as plain values (not modules, classes,
    routines, frames, tracebacks or code)."""
    checks = (inspect.ismodule, inspect.isclass, inspect.isroutine,
              inspect.isframe, inspect.istraceback, inspect.iscode)
    return not any(check(object) for check in checks)


def _is_bound_method(fn):
    """True for methods bound to an instance or class (a builtin function
    bound to a module is a plain function)."""
    if inspect.ismethod(fn):
        return True
    if inspect.isbuiltin(fn):
        bound = getattr(fn, '__self__', None)
        return bound is not None and not inspect.ismodule(bound)
    return False


def allmethods(cl):
    """Dict of all routines of class *cl* (as retrieved from the class)."""
    return {name: getattr(cl, name)
            for name, _ in inspect.getmembers(cl, inspect.isroutine)}


def _split_list(s, predicate):
    """Partition *s* into ([items satisfying predicate], [the others])."""
    chosen, rest = [], []
    for item in s:
        (chosen if predicate(item) else rest).append(item)
    return chosen, rest


_FUTURE_NAMES = frozenset(__future__.all_feature_names)

# Dunder names that never appear in the attribute listings.
_HIDDEN = frozenset((
    '__author__', '__builtins__', '__cached__', '__credits__', '__date__',
    '__doc__', '__file__', '__spec__', '__loader__', '__module__',
    '__name__', '__package__', '__path__', '__qualname__', '__slots__',
    '__version__', '__static_attributes__', '__firstlineno__',
    '__annotate__', '__annotate_func__', '__annotations__',
    '__annotations_cache__', '__lazy_modules__',
))


def visiblename(name, all=None, obj=None):
    """Decide whether attribute *name* of *obj* is shown."""
    if name in _HIDDEN:
        return False
    if name[:2] == '__' and name[-2:] == '__':
        return True
    if name.startswith('_') and hasattr(obj, '_fields'):
        return True          # namedtuple API (_replace, _asdict, ...)
    if (obj is not __future__ and name in _FUTURE_NAMES
            and isinstance(getattr(obj, name, None), __future__._Feature)):
        return False         # "from __future__ import ..." features
    if all is not None:
        return name in all
    return not name.startswith('_')


def classify_class_attrs(object):
    """inspect.classify_class_attrs() with pydoc's categories: data
    descriptors and read-only properties are separated from plain data,
    and bound methods stored in a class count as static methods."""
    result = []
    for name, kind, owner, value in inspect.classify_class_attrs(object):
        if inspect.isdatadescriptor(value):
            if isinstance(value, property) and value.fset is None:
                kind = 'readonly property'
            else:
                kind = 'data descriptor'
        elif kind == 'method' and _is_bound_method(value):
            kind = 'static method'
        result.append((name, kind, owner, value))
    return result


def sort_attributes(attrs, object):
    """Sort (name, kind, cls, value) tuples: namedtuple fields first in
    field order, then the rest alphabetically."""
    fields = getattr(object, '_fields', [])
    try:
        rank = {field: position - len(fields)
                for position, field in enumerate(fields)}
    except TypeError:
        rank = {}
    attrs.sort(key=lambda attr: (rank.get(attr[0], 0), attr[0]))


def _signature_text(obj):
    """Text of the call signature of *obj* (long ones wrapped over
    several lines), or None if it has none."""
    try:
        sig = inspect.signature(obj, annotation_format=Format.STRING)
    except (TypeError, ValueError):
        text = getattr(obj, '__text_signature__', None)
        if not text:
            return text
        if text.startswith('($'):
            text = '(' + text[2:]
        if getattr(obj, '__self__', None) is not None:
            # bound: drop the first parameter (and a '/' right after it)
            m = re.match(r'\(\w+(?:(?=\))|,\s*(?:/(?:(?=\))|,\s*))?)', text)
            if m:
                text = '(' + text[m.end():]
        return text
    if not sig:
        return None
    name = getattr(obj, '__name__', '')
    width = None if name == '<lambda>' else 80 - len(name)
    return sig.format(max_width=width, quote_annotation_strings=False)


# ------------------------------------------------- locating and importing

def ispackage(path):
    """Deprecated: True if directory *path* contains an __init__ module."""
    warnings.warn('The pydoc.ispackage() function is deprecated',
                  DeprecationWarning, stacklevel=2)
    return os.path.isdir(path) and any(
        os.path.isfile(os.path.join(path, '__init__' + ext))
        for ext in ('.py', '.pyc'))


def _first_statement(file):
    """Source text of the first statement of a text file, or None."""
    ignorable = {tokenize.COMMENT, tokenize.NL, tokenize.ENCODING,
                 tokenize.INDENT, tokenize.DEDENT}
    start = end = None
    for tok in tokenize.generate_tokens(file.readline):
        if start is None and tok.type in ignorable:
            continue
        if tok.type in (tokenize.NEWLINE, tokenize.ENDMARKER):
            break
        if start is None:
            start = tok.start
        end = tok.end
    if start is None:
        return None
    file.seek(0)
    lines = file.read().splitlines(keepends=True)
    if lines and lines[0].startswith('﻿'):
        lines[0] = lines[0][1:]
    chunk = lines[start[0] - 1:end[0]]
    if not chunk:
        return None
    chunk[-1] = chunk[-1][:end[1]]
    chunk[0] = chunk[0][start[1]:]
    return ''.join(chunk)


def source_synopsis(file):
    """First line of the module docstring of the source text in *file*
    (an open text file), or None."""
    import ast
    try:
        statement = _first_statement(file)
    except (tokenize.TokenError, SyntaxError, UnicodeDecodeError,
            ValueError, OSError):
        return None
    if statement is None:
        return None
    try:
        with warnings.catch_warnings():
            warnings.simplefilter('ignore')
            tree = ast.parse(statement.strip(), mode='eval')
    except (SyntaxError, ValueError, MemoryError):
        return None
    node = tree.body
    if not isinstance(node, ast.Constant) or not isinstance(node.value, str):
        return None
    return node.value.strip().split('\n')[0].strip()


def synopsis(filename, cache={}):
    """One-line summary of the module in *filename* (source or bytecode);
    results are cached per file modification time."""
    mtime = os.stat(filename).st_mtime
    cached = cache.get(filename)
    if cached is not None and cached[0] >= mtime:
        return cached[1]
    if filename.endswith(tuple(importlib.machinery.BYTECODE_SUFFIXES)):
        loader_class = importlib.machinery.SourcelessFileLoader
    elif filename.endswith(tuple(importlib.machinery.EXTENSION_SUFFIXES)):
        loader_class = importlib.machinery.ExtensionFileLoader
    else:
        loader_class = None
    if loader_class is None:
        try:
            with tokenize.open(filename) as file:
                result = source_synopsis(file)
        except (OSError, SyntaxError, UnicodeDecodeError):
            return None
    else:
        tmpname = '__temp__'
        spec = importlib.util.spec_from_file_location(
            tmpname, filename, loader=loader_class(tmpname, filename))
        try:
            module = importlib._bootstrap._load(spec)
        except Exception:
            return None
        sys.modules.pop(tmpname, None)
        text = module.__doc__
        result = text.splitlines()[0] if text else None
    cache[filename] = (mtime, result)
    return result


class ErrorDuringImport(Exception):
    """An exception raised while importing the module to document."""

    def __init__(self, filename, exc_info):
        if isinstance(exc_info, tuple):
            warnings.warn("A tuple value for exc_info is deprecated, "
                          "use an exception instance", DeprecationWarning)
            self.exc, self.value, self.tb = exc_info
        else:
            self.exc = type(exc_info)
            self.value = exc_info
            self.tb = exc_info.__traceback__
        self.filename = filename

    def __str__(self):
        return f'problem in {self.filename} - {self.exc.__name__}: {self.value}'


def importfile(path):
    """Import the source or bytecode file *path* as a module named after
    the file."""
    magic = importlib.util.MAGIC_NUMBER
    with open(path, 'rb') as file:
        compiled = file.read(len(magic)) == magic
    name = os.path.splitext(os.path.basename(path))[0]
    external = importlib._bootstrap_external
    loader_class = (external.SourcelessFileLoader if compiled
                    else external.SourceFileLoader)
    spec = importlib.util.spec_from_file_location(
        name, path, loader=loader_class(name, path))
    try:
        return importlib._bootstrap._load(spec)
    except BaseException as exc:
        raise ErrorDuringImport(path, exc)


def safeimport(path, forceload=0, cache={}):
    """Import the module *path*; return None if it does not exist.

    Errors raised while the module itself runs are wrapped in
    ErrorDuringImport.  With *forceload*, a loaded (non-builtin) module and
    its submodules are re-imported from source.
    """
    if forceload and path in sys.modules and path not in sys.builtin_module_names:
        stale = [name for name in sys.modules
                 if name == path or name.startswith(path + '.')]
        for name in stale:
            cache[name] = sys.modules.pop(name)
    try:
        return importlib.import_module(path)
    except BaseException as exc:
        if path in sys.modules:
            # the module was found but failed while executing
            raise ErrorDuringImport(sys.modules[path].__file__, exc)
        if type(exc) is SyntaxError:
            raise ErrorDuringImport(exc.filename, exc)
        if isinstance(exc, ImportError) and exc.name == path:
            return None
        raise ErrorDuringImport(path, exc)


def locate(path, forceload=0):
    """Find the object named by the dotted *path*, importing the longest
    module prefix; builtins are searched when no module matches."""
    parts = [part for part in path.split('.') if part]
    found, used = None, 0
    while used < len(parts):
        module = safeimport('.'.join(parts[:used + 1]), forceload)
        if not module:
            break
        found, used = module, used + 1
    target = found if found else builtins
    for part in parts[used:]:
        try:
            target = getattr(target, part)
        except AttributeError:
            return None
    return target


# --------------------------------------------------- shared doc structure

def _routine_summary(obj, name, mod, cl=None, homecls=None):
    """Facts needed to render a routine: a dict with realname, name,
    note (e.g. ' method of X instance'), alias (realname if documented under
    another name), skipdocs, argspec and async."""
    realname = getattr(obj, '__name__', None)
    if not isinstance(realname, str):
        realname = None
    name = name or realname
    if homecls is None:
        homecls = cl
    note = ''
    plain_func = None
    if _is_bound_method(obj):
        bound = obj.__self__
        if bound is cl:
            plain_func = getattr(obj, '__func__', None)
        elif inspect.isclass(bound):
            note = f' class method of {classname(bound, mod)}'
        else:
            note = f' method of {classname(type(bound), mod)} instance'
    elif inspect.ismethoddescriptor(obj) or inspect.ismethodwrapper(obj):
        owner = getattr(obj, '__objclass__', None)
        if owner is not None:
            if cl is None:
                note = f' unbound {classname(owner, mod)} method'
            elif owner is not homecls:
                note = ' from ' + classname(owner, mod)
    else:
        plain_func = obj
    if (inspect.isfunction(plain_func) and homecls is not None
            and (plain_func.__module__ != homecls.__module__
                 or plain_func.__qualname__ != f'{homecls.__qualname__}.{realname}')):
        scope = _scope_name(plain_func, mod)
        if scope:
            note = ' from ' + scope

    skipdocs = False
    alias = None
    if realname is not None and name != realname:
        alias = realname
        if cl is not None and inspect.getattr_static(cl, realname, []) is obj:
            # documented in full under its own name in the same class
            skipdocs = True
            if note.startswith(' from '):
                note = ''

    argspec = None
    is_lambda = False
    if realname is not None and inspect.isroutine(obj):
        argspec = _signature_text(obj)
        if argspec and realname == '<lambda>':
            is_lambda = True
            if not getattr(obj, '__annotations__', None):
                argspec = argspec[1:-1]
    if realname is not None and not argspec:
        argspec = '(...)'
    return {
        'realname': realname,
        'name': name,
        'alias': alias,
        'note': note,
        'skipdocs': skipdocs,
        'argspec': argspec or '',
        'lambda': is_lambda,
        'async': (inspect.iscoroutinefunction(obj)
                  or inspect.isasyncgenfunction(obj)),
    }


# Section kinds of a class page, in display order.
_CLASS_SECTIONS = (
    ('method', 'Methods'),
    ('class method', 'Class methods'),
    ('static method', 'Static methods'),
    ('readonly property', 'Readonly properties'),
    ('data descriptor', 'Data descriptors'),
    ('data', 'Data and other attributes'),
)


def _class_groups(cls):
    """Yield (origin, [(name, kind, owner, value), ...]) per class that
    contributes attributes to *cls*: first along the MRO, then classes
    outside it (metaclasses providing virtual attributes).  origin is None
    for attributes defined by *cls* itself.  Members inherited from
    object are left out (unless *cls* is object)."""
    attrs = [entry for entry in classify_class_attrs(cls)
             if visiblename(entry[0], obj=cls)]
    pending = deque(inspect.getmro(cls))
    while attrs:
        origin = pending.popleft() if pending else attrs[0][2]
        mine, attrs = _split_list(attrs, lambda entry: entry[2] is origin)
        if origin is builtins.object and cls is not builtins.object:
            continue
        if mine:
            sort_attributes(mine, cls)
            yield (None if origin is cls else origin), mine


def _builtin_subclasses(cls):
    """Names of the public builtin direct subclasses of *cls*, sorted
    case-insensitively (empty for metaclasses)."""
    if issubclass(cls, type):
        return []
    names = [sub.__name__ for sub in type.__subclasses__(cls)
             if not sub.__name__.startswith('_')
             and getattr(sub, '__module__', '') == 'builtins']
    return sorted(names, key=str.lower)


def _attribute_value(cls, name, fallback):
    """getattr(cls, name), or *fallback* when the descriptor fails."""
    try:
        return getattr(cls, name), True
    except Exception:
        return fallback, False


# ------------------------------------------------------- formatter base

class Doc:
    """Base of the documentation formatters: dispatches on the kind of
    object."""

    PYTHONDOCS = os.environ.get(
        "PYTHONDOCS",
        "https://docs.python.org/%d.%d/library" % sys.version_info[:2])

    def document(self, object, name=None, *args):
        """Document *object* with the method suited to its kind."""
        args = (object, name) + args
        try:
            if inspect.ismodule(object):
                return self.docmodule(*args)
            if inspect.isclass(object):
                return self.docclass(*args)
            if inspect.isroutine(object):
                return self.docroutine(*args)
        except AttributeError:
            pass
        if inspect.isdatadescriptor(object):
            return self.docdata(*args)
        return self.docother(*args)

    def fail(self, object, name=None, *args):
        """Report a kind of object this formatter cannot document."""
        label = f' {name!r}' if name else ''
        raise TypeError(f"don't know how to document object{label} "
                        f"of type {type(object).__name__}")

    docmodule = docclass = docroutine = docother = docproperty = docdata = fail

    def _get_version(self, object):
        """The __version__ of *object* as a string (None if absent)."""
        if inspect.ismodule(object):
            # modules of the standard library deprecate the attribute
            with warnings.catch_warnings():
                warnings.simplefilter('ignore', DeprecationWarning)
                value = getattr(object, '__version__', None)
        else:
            value = getattr(object, '__version__', None)
        if value is None:
            return None
        value = str(value)
        if value.startswith('$' + 'Revision: ') and value.endswith('$'):
            value = value[11:-1].strip()
        return value

    def getdocloc(self, object, basedir=None):
        """URL of the library reference page of a standard module, or
        None for other objects."""
        if not isinstance(object, type(os)):
            return None
        if object.__name__ in ('xml.etree', 'test.test_pydoc.pydoc_mod'):
            return None
        if basedir is None:
            basedir = sysconfig.get_path('stdlib')
        try:
            filename = os.path.normcase(inspect.getabsfile(object))
        except TypeError:
            filename = '(built-in)'
        roots = {os.path.normcase(basedir)}
        if getattr(sys, '_stdlib_dir', None):
            roots.add(os.path.normcase(sys._stdlib_dir))
        in_stdlib = any(
            filename.startswith(root + os.sep)
            and not filename.startswith(os.path.join(root, 'site-packages'))
            for root in roots if root)
        builtin_like = object.__name__ in (
            'errno', 'exceptions', 'gc', 'marshal', 'posix', 'signal',
            'sys', '_thread', 'zipimport')
        if not (in_stdlib or builtin_like):
            return None
        try:
            from pydoc_data import module_docs
        except ImportError:
            return None
        page = module_docs.module_docs.get(object.__name__)
        if page is None:
            return None
        location = os.environ.get("PYTHONDOCS", self.PYTHONDOCS)
        if location.startswith(('http://', 'https://')):
            return f'{location.rstrip("/")}/{page}'
        return os.path.join(location, page)


# --------------------------------------------------------------- text

class TextRepr(Repr):
    """repr() with size limits for text documentation."""

    def __init__(self):
        Repr.__init__(self)
        self.maxlist = self.maxtuple = 20
        self.maxdict = 10
        self.maxstring = self.maxother = 100

    def repr1(self, x, level):
        handler = getattr(self, 'repr_' + '_'.join(type(x).__name__.split()), None)
        if handler is not None:
            return handler(x, level)
        return cram(stripid(repr(x)), self.maxother)

    def repr_string(self, x, level):
        shortened = cram(x, self.maxstring)
        text = repr(shortened)
        if '\\' in shortened and '\\' not in text.replace(r'\\', ''):
            # only literal backslashes: a raw string reads better
            return 'r' + text[0] + shortened + text[0]
        return text

    repr_str = repr_string

    def repr_instance(self, x, level):
        try:
            return cram(stripid(repr(x)), self.maxstring)
        except Exception:
            return f'<{type(x).__name__} instance>'


class TextDoc(Doc):
    """Formats documentation as text (bold by overstriking)."""

    _repr_instance = TextRepr()
    repr = _repr_instance.repr

    def bold(self, text):
        """Overstrike every character (shown bold by pagers)."""
        return ''.join(char + '\b' + char for char in text)

    def indent(self, text, prefix='    '):
        """Prefix every line of *text* (trailing blanks removed)."""
        if not text:
            return ''
        return '\n'.join((prefix + line).rstrip() for line in text.split('\n'))

    def section(self, title, contents):
        """A titled block of indented text followed by a blank line."""
        return self.bold(title) + '\n' + self.indent(contents).rstrip() + '\n\n'

    def formattree(self, tree, modname, parent=None, prefix=''):
        """Class tree (from inspect.getclasstree) as indented lines."""
        lines = []
        last = None
        for entry in tree:
            if isinstance(entry, tuple):
                cls, bases = entry
                last = cls
                line = prefix + classname(cls, modname)
                if bases and bases != (parent,):
                    line += '(%s)' % ', '.join(classname(b, modname) for b in bases)
                lines.append(line + '\n')
            elif isinstance(entry, list):
                lines.append(self.formattree(entry, modname, last, prefix + '    '))
        return ''.join(lines)

    def docmodule(self, object, name=None, mod=None, *ignored):
        """Text documentation of a module."""
        name = object.__name__
        synop, desc = splitdoc(getdoc(object))
        parts = [self.section('NAME', name + (synop and ' - ' + synop))]
        location = self.getdocloc(object)
        if location is not None:
            parts.append(self.section(
                'MODULE REFERENCE', location + '\n\n'
                'This page is generated from the source and docstrings; '
                'the reference\nmanual at the location above is the '
                'authoritative documentation.\n'))
        if desc:
            parts.append(self.section('DESCRIPTION', desc))

        members = _module_members(object)
        if hasattr(object, '__path__'):
            entries = sorted(
                modname + (' (package)' if ispkg else '')
                for _, modname, ispkg in pkgutil.iter_modules(object.__path__))
            parts.append(self.section('PACKAGE CONTENTS', '\n'.join(entries)))
            contents = {modname for _, modname, _ in pkgutil.iter_modules(object.__path__)}
        else:
            contents = set()
        submodules = sorted(
            key for key, value in inspect.getmembers(object, inspect.ismodule)
            if value.__name__.startswith(name + '.') and key not in contents)
        if submodules:
            parts.append(self.section('SUBMODULES', '\n'.join(submodules)))

        if members['classes']:
            tree = inspect.getclasstree([cls for _, cls in members['classes']], 1)
            blocks = [self.formattree(tree, name)]
            blocks += [self.document(value, key, name)
                       for key, value in members['classes']]
            parts.append(self.section('CLASSES', '\n'.join(blocks)))
        if members['functions']:
            blocks = [self.document(value, key, name)
                      for key, value in members['functions']]
            parts.append(self.section('FUNCTIONS', '\n'.join(blocks)))
        if members['data']:
            blocks = [self.docother(value, key, name, maxlen=70)
                      for key, value in members['data']]
            parts.append(self.section('DATA', '\n'.join(blocks)))

        version = self._get_version(object)
        if version:
            parts.append(self.section('VERSION', version))
        for attr, title in (('__date__', 'DATE'), ('__author__', 'AUTHOR'),
                            ('__credits__', 'CREDITS')):
            if hasattr(object, attr):
                parts.append(self.section(title, str(getattr(object, attr))))
        try:
            filename = inspect.getabsfile(object)
        except TypeError:
            filename = '(built-in)'
        parts.append(self.section('FILE', filename))
        return ''.join(parts)

    def docclass(self, object, name=None, mod=None, *ignored):
        """Text documentation of a class ('|' marks its body)."""
        realname = object.__name__
        name = name or realname
        module = object.__module__
        if name == realname:
            title = 'class ' + self.bold(realname)
        else:
            title = self.bold(name) + ' = class ' + realname
        if object.__bases__:
            title += '(%s)' % ', '.join(classname(b, module) for b in object.__bases__)

        lines = []
        argspec = _signature_text(object)
        if argspec and argspec != '()':
            lines.append(name + argspec + '\n')
        doc = getdoc(object)
        if doc:
            lines.append(doc + '\n')
        mro = inspect.getmro(object)
        if len(mro) > 2:
            lines.append('Method resolution order:')
            lines.extend('    ' + classname(base, module) for base in mro)
            lines.append('')
        subclasses = _builtin_subclasses(object)
        if subclasses:
            lines.append('Built-in subclasses:')
            lines.extend('    ' + sub for sub in subclasses[:4])
            if len(subclasses) > 4:
                lines.append(f'    ... and {len(subclasses) - 4} other subclasses')
            lines.append('')

        first = True
        for origin, attrs in _class_groups(object):
            where = ('defined here' if origin is None
                     else 'inherited from ' + classname(origin, module))
            for kind, label in _CLASS_SECTIONS:
                chosen = [a for a in attrs if a[1] == kind]
                if not chosen:
                    continue
                if not first:
                    lines.append('-' * 70)
                first = False
                lines.append(f'{label} {where}:\n')
                for attr_name, _, owner, value in chosen:
                    lines.append(self._class_member(object, attr_name, kind,
                                                    owner, value, mod))
        body = '\n'.join(lines)
        if not body:
            return title + '\n'
        return title + '\n' + self.indent(body.rstrip(), ' |  ') + '\n'

    def _class_member(self, cls, attr_name, kind, owner, value, mod):
        if kind in ('readonly property', 'data descriptor'):
            return self.docdata(value, attr_name, mod)
        if kind == 'data':
            fetched, ok = _attribute_value(cls, attr_name, None)
            if not ok:
                fetched = owner.__dict__.get(attr_name, value)
            return self.docother(fetched, attr_name, mod, maxlen=70,
                                 doc=getdoc(value)) + '\n'
        fetched, ok = _attribute_value(cls, attr_name, value)
        if not ok:
            return self.docdata(value, attr_name, mod)
        return self.document(fetched, attr_name, mod, cls, owner)

    def formatvalue(self, object):
        """Text of a default value in a signature."""
        return '=' + self.repr(object)

    def docroutine(self, object, name=None, mod=None, cl=None, homecls=None):
        """Text documentation of a function or method."""
        if not isinstance(getattr(object, '__name__', None), str):
            # routine-like object without a name: describe it as a value
            return self.docother(object, name, mod)
        info = _routine_summary(object, name, mod, cl, homecls)
        if info['lambda']:
            title = self.bold(info['name']) + ' lambda '
        elif info['alias']:
            title = self.bold(info['name']) + ' = ' + info['alias']
        else:
            title = self.bold(info['realname'])
        decl = (('async ' if info['async'] else '') + title
                + info['argspec'] + info['note'])
        if info['skipdocs']:
            return decl + '\n'
        doc = getdoc(object)
        return decl + '\n' + (self.indent(doc).rstrip() + '\n' if doc else '')

    def docdata(self, object, name=None, mod=None, cl=None, *ignored):
        """Text documentation of a data descriptor (e.g. a property)."""
        out = []
        if name:
            out.append(self.bold(name) + '\n')
        doc = getdoc(object)
        if doc:
            out.append(self.indent(doc) + '\n')
        return ''.join(out)

    docproperty = docdata

    def docother(self, object, name=None, mod=None, parent=None, *ignored,
                 maxlen=None, doc=None):
        """Text documentation of a value: 'name = repr'."""
        value = self.repr(object)
        if maxlen:
            overflow = len((name + ' = ' if name else '') + value) - maxlen
            if overflow > 0:
                value = value[:-overflow] + '...'
        line = (self.bold(name) + ' = ' if name else '') + value
        if not doc:
            doc = getdoc(object)
        if doc:
            line += '\n' + self.indent(str(doc)) + '\n'
        return line


class _PlainTextDoc(TextDoc):
    """TextDoc without overstriking."""

    def bold(self, text):
        return text


def _module_members(module):
    """The classes, functions and data a module page lists."""
    names = getattr(module, '__all__', None)

    def belongs(value):
        return names is not None or (inspect.getmodule(value) or module) is module

    classes = [(key, value)
               for key, value in inspect.getmembers(module, inspect.isclass)
               if belongs(value) and visiblename(key, names, module)]
    functions = [(key, value)
                 for key, value in inspect.getmembers(module, inspect.isroutine)
                 if belongs(value) and visiblename(key, names, module)]
    data = [(key, value)
            for key, value in inspect.getmembers(module, isdata)
            if visiblename(key, names, module)]
    return {'classes': classes, 'functions': functions, 'data': data}


# --------------------------------------------------------------- HTML

class HTMLRepr(Repr):
    """repr() with size limits, HTML-escaped."""

    def __init__(self):
        Repr.__init__(self)
        self.maxlist = self.maxtuple = 20
        self.maxdict = 10
        self.maxstring = self.maxother = 100

    def escape(self, text):
        return replace(text, '&', '&amp;', '<', '&lt;', '>', '&gt;')

    def repr(self, object):
        return Repr.repr(self, object)

    def repr1(self, x, level):
        handler = getattr(self, 'repr_' + '_'.join(type(x).__name__.split()), None)
        if handler is not None:
            return handler(x, level)
        return self.escape(cram(stripid(repr(x)), self.maxother))

    def repr_string(self, x, level):
        shortened = cram(x, self.maxstring)
        text = repr(shortened)
        if '\\' in shortened and '\\' not in text.replace(r'\\', ''):
            return 'r' + text[0] + self.escape(shortened) + text[0]
        return re.sub(r'((\\[\\abfnrtv\'"]|\\[0-9]..|\\x..|\\u....)+)',
                      r'<span class="repr">\1</span>', self.escape(text))

    repr_str = repr_string

    def repr_instance(self, x, level):
        try:
            return self.escape(cram(stripid(repr(x)), self.maxstring))
        except Exception:
            return self.escape(f'<{type(x).__name__} instance>')


class HTMLDoc(Doc):
    """Formats documentation as HTML."""

    _repr_instance = HTMLRepr()
    repr = _repr_instance.repr
    escape = _repr_instance.escape

    def page(self, title, contents):
        """A complete HTML page."""
        return (f'<!DOCTYPE html>\n<html lang="en">\n<head>\n'
                f'<meta charset="utf-8">\n<title>Python: {title}</title>\n'
                f'</head><body>\n{contents}\n</body></html>')

    def heading(self, title, extras=''):
        """Page heading: title on the left, links on the right."""
        return (f'\n<table class="heading">\n'
                f'<tr class="heading-text decor">\n'
                f'<td class="title">&nbsp;<br>{title}</td>\n'
                f'<td class="extra">{extras or "&nbsp;"}</td></tr></table>\n')

    def section(self, title, cls, contents, width=6, prelude='',
                marginalia=None, gap='&nbsp;'):
        """A titled section table."""
        if marginalia is None:
            marginalia = '<span class="code">' + '&nbsp;' * width + '</span>'
        out = [f'<p>\n<table class="section">\n'
               f'<tr class="decor {cls}-decor heading-text">\n'
               f'<td class="section-title" colspan=3>&nbsp;<br>{title}</td></tr>\n']
        if prelude:
            out.append(f'<tr><td class="decor {cls}-decor" rowspan=2>{marginalia}</td>\n'
                       f'<td class="decor {cls}-decor" colspan=2>{prelude}</td></tr>\n'
                       f'<tr><td>{gap}</td>')
        else:
            out.append(f'<tr><td class="decor {cls}-decor">{marginalia}</td><td>{gap}</td>')
        out.append(f'\n<td class="singlecolumn">{contents}</td></tr></table>')
        return ''.join(out)

    def bigsection(self, title, *args):
        """A section with a big title."""
        return self.section(f'<strong class="bigsection">{title}</strong>', *args)

    def preformat(self, text):
        """Escape text keeping its line breaks and spacing."""
        text = self.escape(text.expandtabs())
        text = text.replace('\n\n', '\n \n').replace('\n\n', '\n \n')
        return text.replace(' ', '&nbsp;').replace('\n', '<br>\n')

    def multicolumn(self, list, format):
        """Lay out *list* in four columns, formatting items with *format*."""
        rows = (len(list) + 3) // 4
        cells = []
        for column in range(4):
            items = list[column * rows:(column + 1) * rows]
            cells.append('<td class="multicolumn">'
                         + ''.join(format(item) + '<br>\n' for item in items)
                         + '</td>')
        return '<table><tr>%s</tr></table>' % ''.join(cells)

    def grey(self, text):
        return f'<span class="grey">{text}</span>'

    def namelink(self, name, *dicts):
        """Link *name* using the first mapping that knows it."""
        for mapping in dicts:
            if name in mapping:
                return f'<a href="{mapping[name]}">{name}</a>'
        return name

    def classlink(self, object, modname):
        """Link to the documentation of a class."""
        module = sys.modules.get(getattr(object, '__module__', None))
        name = object.__name__
        if module is not None and getattr(module, name, None) is object:
            return f'<a href="{module.__name__}.html#{name}">{classname(object, modname)}</a>'
        return classname(object, modname)

    def parentlink(self, object, modname):
        """Link to the class or module enclosing *object*."""
        scope = _scope_name(object, modname)
        module = sys.modules.get(getattr(object, '__module__', None))
        if module is None or getattr(module, object.__name__, None) is not object:
            return scope
        qualname = object.__qualname__
        if '.' in qualname:
            anchor = qualname.rpartition('.')[0]
            href = (f'{module.__name__}.html#{anchor}'
                    if object.__module__ != modname else f'#{anchor}')
        elif object.__module__ != modname:
            href = f'{module.__name__}.html'
        else:
            return scope
        return f'<a href="{href}">{scope}</a>'

    def modulelink(self, object):
        return f'<a href="{object.__name__}.html">{object.__name__}</a>'

    def modpkglink(self, modpkginfo):
        """Link for (name, path, ispackage, shadowed) in an index."""
        name, path, is_package, shadowed = modpkginfo
        if shadowed:
            return self.grey(name)
        url = f'{path}.{name}.html' if path else f'{name}.html'
        label = f'<strong>{name}</strong>&nbsp;(package)' if is_package else name
        return f'<a href="{url}">{label}</a>'

    def filelink(self, url, path):
        return f'<a href="file:{url}">{path}</a>'

    _LINKABLE = re.compile(r'\b((http|https|ftp)://\S+[\w/]|'
                           r'RFC[- ]?(\d+)|'
                           r'PEP[- ]?(\d+)|'
                           r'(self\.)?(\w+))')

    def markup(self, text, escape=None, funcs={}, classes={}, methods={}):
        """Escape *text*, turning URLs, RFC/PEP numbers and known names
        into links."""
        escape = escape or self.escape
        out = []
        pos = 0
        for match in self._LINKABLE.finditer(text):
            start, end = match.span()
            out.append(escape(text[pos:start]))
            whole, scheme, rfc, pep, selfdot, name = match.groups()
            if scheme:
                url = escape(whole).replace('"', '&quot;')
                out.append(f'<a href="{url}">{url}</a>')
            elif rfc:
                out.append(f'<a href="https://www.rfc-editor.org/rfc/rfc{int(rfc)}.txt">'
                           f'{escape(whole)}</a>')
            elif pep:
                out.append(f'<a href="https://peps.python.org/pep-{int(pep):04d}/">'
                           f'{escape(whole)}</a>')
            elif selfdot:
                if text[end:end + 1] == '(':
                    out.append('self.' + self.namelink(name, methods))
                else:
                    out.append(f'self.<strong>{name}</strong>')
            elif text[end:end + 1] == '(':
                out.append(self.namelink(name, methods, funcs, classes))
            else:
                out.append(self.namelink(name, classes))
            pos = end
        out.append(escape(text[pos:]))
        return ''.join(out)

    def formattree(self, tree, modname, parent=None):
        """Class tree as nested definition lists."""
        out = []
        last = None
        for entry in tree:
            if isinstance(entry, tuple):
                cls, bases = entry
                last = cls
                item = '<dt class="heading-text">' + self.classlink(cls, modname)
                if bases and bases != (parent,):
                    item += '(%s)' % ', '.join(self.classlink(b, modname) for b in bases)
                out.append(item + '\n</dt>')
            elif isinstance(entry, list):
                out.append('<dd>\n%s</dd>\n' % self.formattree(entry, modname, last))
        return '<dl>\n%s</dl>\n' % ''.join(out)

    def _file_link(self, object):
        try:
            path = inspect.getabsfile(object)
        except TypeError:
            return '(built-in)'
        try:
            import urllib.request
            url = urllib.request.pathname2url(path)
        except Exception:
            try:
                import urllib.parse
                url = urllib.parse.quote(path)
            except ImportError:
                url = path
        return self.filelink(url, self.escape(path))

    def docmodule(self, object, name=None, mod=None, *ignored):
        """HTML documentation of a module."""
        name = object.__name__
        parts = name.split('.')
        links = [f'<a href="{".".join(parts[:i + 1])}.html" class="white">{parts[i]}</a>'
                 for i in range(len(parts) - 1)]
        head = '<strong class="title">%s</strong>' % '.'.join(links + parts[-1:])
        info = []
        version = self._get_version(object)
        if version:
            info.append('version ' + self.escape(version))
        if hasattr(object, '__date__'):
            info.append(self.escape(str(object.__date__)))
        if info:
            head += ' (%s)' % ', '.join(info)
        extras = '<a href=".">index</a><br>' + self._file_link(object)
        location = self.getdocloc(object)
        if location is not None:
            extras += f'<br><a href="{location}">Module Reference</a>'
        out = [self.heading(head, extras)]

        members = _module_members(object)
        cdict = {}
        for key, value in members['classes']:
            cdict[key] = cdict[value] = '#' + key
        for key, value in members['classes']:
            for base in value.__bases__:
                base_mod = getattr(base, '__module__', None)
                module = sys.modules.get(base_mod)
                if (base_mod != name and module is not None
                        and getattr(module, base.__name__, None) is base
                        and base.__name__ not in cdict):
                    cdict[base.__name__] = cdict[base] = f'{base_mod}.html#{base.__name__}'
        fdict = {}
        for key, value in members['functions']:
            fdict[key] = '#-' + key
            if inspect.isfunction(value):
                fdict[value] = fdict[key]

        doc = self.markup(getdoc(object), self.preformat, fdict, cdict)
        if doc:
            doc = f'<span class="code">{doc}</span>'
        out.append(f'<p>{doc}</p>\n')

        if hasattr(object, '__path__'):
            entries = sorted((modname, name, ispkg, 0) for _, modname, ispkg
                             in pkgutil.iter_modules(object.__path__))
            out.append(self.bigsection('Package Contents', 'pkg-content',
                                       self.multicolumn(entries, self.modpkglink)))
        else:
            modules = [(key, value) for key, value
                       in inspect.getmembers(object, inspect.ismodule)
                       if key != '__builtins__']
            if modules:
                out.append(self.bigsection(
                    'Modules', 'pkg-content',
                    self.multicolumn(modules, lambda item: self.modulelink(item[1]))))

        if members['classes']:
            tree = inspect.getclasstree([cls for _, cls in members['classes']], 1)
            blocks = [self.formattree(tree, name)]
            blocks += [self.document(value, key, name, fdict, cdict)
                       for key, value in members['classes']]
            out.append(self.bigsection('Classes', 'index', ' '.join(blocks)))
        if members['functions']:
            blocks = [self.document(value, key, name, fdict, cdict)
                      for key, value in members['functions']]
            out.append(self.bigsection('Functions', 'functions', ' '.join(blocks)))
        if members['data']:
            blocks = [self.document(value, key) for key, value in members['data']]
            out.append(self.bigsection('Data', 'data', '<br>\n'.join(blocks)))
        for attr, title, cls in (('__author__', 'Author', 'author'),
                                 ('__credits__', 'Credits', 'credits')):
            if hasattr(object, attr):
                text = self.markup(str(getattr(object, attr)), self.preformat)
                out.append(self.bigsection(title, cls, text))
        return ''.join(out)

    def docclass(self, object, name=None, mod=None, funcs={}, classes={},
                 *ignored):
        """HTML documentation of a class."""
        realname = object.__name__
        name = name or realname
        module = object.__module__

        mdict = {}
        for attr_name, _, _, value in classify_class_attrs(object):
            if not visiblename(attr_name, obj=object):
                continue
            anchor = f'#{name}-{attr_name}'
            mdict[attr_name] = anchor
            try:
                mdict[value] = anchor
            except TypeError:
                pass

        out = []
        rule = False
        mro = inspect.getmro(object)
        if len(mro) > 2:
            rule = True
            out.append('<dl><dt>Method resolution order:</dt>\n')
            out.extend(f'<dd>{self.classlink(base, module)}</dd>\n' for base in mro)
            out.append('</dl>\n')

        for origin, attrs in _class_groups(object):
            where = ('defined here' if origin is None
                     else 'inherited from ' + self.classlink(origin, module))
            for kind, label in _CLASS_SECTIONS:
                chosen = [a for a in attrs if a[1] == kind]
                if not chosen:
                    continue
                if rule:
                    out.append('<hr>\n')
                rule = True
                out.append(f'{label} {where}:<br>\n')
                for attr_name, _, owner, value in chosen:
                    out.append(self._class_member(object, attr_name, kind, owner,
                                                  value, mod, funcs, classes, mdict))

        if name == realname:
            title = f'<a name="{name}">class <strong>{realname}</strong></a>'
        else:
            title = f'<strong>{name}</strong> = <a name="{name}">class {realname}</a>'
        if object.__bases__:
            title += '(%s)' % ', '.join(self.classlink(b, module)
                                        for b in object.__bases__)
        doc = getdoc(object)
        argspec = _signature_text(object)
        if argspec and argspec != '()':
            doc = name + argspec + '\n\n' + (doc or '')
        doc = self.markup(doc, self.preformat, funcs, classes, mdict)
        if doc:
            doc = f'<span class="code">{doc}<br>&nbsp;</span>'
        return self.section(title, 'title', ''.join(out), 3, doc)

    def _class_member(self, cls, attr_name, kind, owner, value, mod,
                      funcs, classes, mdict):
        if kind in ('readonly property', 'data descriptor'):
            return self.docdata(value, attr_name, mod)
        if kind == 'data':
            fetched, ok = _attribute_value(cls, attr_name, None)
            if not ok:
                fetched = owner.__dict__.get(attr_name, value)
            base = self.docother(fetched, attr_name, mod)
            doc = getdoc(value)
            if not doc:
                return f'<dl><dt>{base}</dl>\n\n'
            doc = self.markup(doc, self.preformat, funcs, classes, mdict)
            return f'<dl><dt>{base}<dd><span class="code">{doc}</span></dl>\n\n'
        fetched, ok = _attribute_value(cls, attr_name, value)
        if not ok:
            return self.docdata(value, attr_name, mod)
        return self.document(fetched, attr_name, mod, funcs, classes,
                             mdict, cls, owner) + '\n'

    def formatvalue(self, object):
        return self.grey('=' + self.repr(object))

    def docroutine(self, object, name=None, mod=None, funcs={}, classes={},
                   methods={}, cl=None, homecls=None):
        """HTML documentation of a function or method."""
        if not isinstance(getattr(object, '__name__', None), str):
            return self.docother(object, name, mod)
        info = _routine_summary(object, name, mod, cl, homecls)
        anchor = ('' if cl is None else cl.__name__) + '-' + (info['name'] or '')
        if info['lambda']:
            title = f'<strong>{info["name"]}</strong> <em>lambda</em> '
        elif info['alias']:
            if info['skipdocs']:
                target = f'<a href="#{cl.__name__}-{info["alias"]}">{info["alias"]}</a>'
            else:
                target = info['alias']
            title = f'<a name="{anchor}"><strong>{info["name"]}</strong></a> = {target}'
        else:
            title = f'<a name="{anchor}"><strong>{info["realname"]}</strong></a>'
        note = info['note']
        if note:
            note = self.grey(f'<span class="heading-text">{self.escape(note)}</span>')
        decl = (('async ' if info['async'] else '') + title
                + self.escape(info['argspec']) + note)
        if info['skipdocs']:
            return f'<dl><dt>{decl}</dt></dl>\n'
        doc = self.markup(getdoc(object), self.preformat, funcs, classes, methods)
        if doc:
            doc = f'<dd><span class="code">{doc}</span></dd>'
        return f'<dl><dt>{decl}</dt>{doc}</dl>\n'

    def docdata(self, object, name=None, mod=None, cl=None, *ignored):
        """HTML documentation of a data descriptor."""
        out = []
        if name:
            out.append(f'<dl><dt><strong>{name}</strong></dt>\n')
        doc = self.markup(getdoc(object), self.preformat)
        if doc:
            out.append(f'<dd><span class="code">{doc}</span></dd>\n')
        out.append('</dl>\n')
        return ''.join(out)

    docproperty = docdata

    def docother(self, object, name=None, mod=None, *ignored):
        """HTML documentation of a value: 'name = repr'."""
        prefix = f'<strong>{name}</strong> = ' if name else ''
        return prefix + self.repr(object)

    def index(self, dir, shadowed=None):
        """Index of the modules found in directory *dir*."""
        if shadowed is None:
            shadowed = {}
        entries = []
        for _, name, ispkg in pkgutil.iter_modules([dir]):
            if any(0xD800 <= ord(ch) <= 0xDFFF for ch in name):
                continue
            entries.append((name, '', ispkg, name in shadowed))
            shadowed[name] = 1
        entries.sort()
        return self.bigsection(dir, 'index', self.multicolumn(entries, self.modpkglink))


# ------------------------------------------------------------- pagers

def _escape_stdout(text):
    """Make *text* writable to sys.stdout (unencodable characters become
    backslash escapes)."""
    encoding = getattr(sys.stdout, 'encoding', None) or 'utf-8'
    return text.encode(encoding, 'backslashreplace').decode(encoding)


def plain_pager(text, title=''):
    """Write the text to stdout without paging."""
    sys.stdout.write(plain(_escape_stdout(text)))


plainpager = plain_pager


def pipe_pager(text, cmd, title=''):
    """Show the text with an external pager command."""
    import shlex
    import subprocess
    env = os.environ.copy()
    if title:
        escaped = title.replace('\\', '\\\\').replace(':', '\\:')
        prompt = (f' {escaped} ?ltline %lt?L/%L.:byte %bB?s/%s..'
                  '?e (END):?pB %pB\\%.. (press h for help or q to quit)')
        env['LESS'] = '-RmPm{0}$PM{0}$'.format(prompt)
    try:
        proc = subprocess.Popen(shlex.split(cmd), stdin=subprocess.PIPE,
                                errors='backslashreplace', env=env)
    except OSError:
        plain_pager(text, title)
        return
    try:
        with proc.stdin as pipe:
            try:
                pipe.write(text)
            except KeyboardInterrupt:
                pass
    except OSError:
        pass  # the user quit the pager early
    while True:
        try:
            proc.wait()
            break
        except KeyboardInterrupt:
            pass  # the pager handles ^C itself


def tty_pager(text, title=''):
    """Minimal built-in pager for a terminal: SPACE/RETURN/b/q."""
    lines = plain(_escape_stdout(text)).split('\n')
    try:
        import termios
        import tty
        fd = sys.stdin.fileno()
        saved = termios.tcgetattr(fd)
        tty.setcbreak(fd)
        read_key = lambda: sys.stdin.read(1)
    except (ImportError, AttributeError, OSError, io.UnsupportedOperation):
        saved = None
        read_key = lambda: sys.stdin.readline()[:-1][:1]
    try:
        try:
            height = int(os.environ.get('LINES', 0))
        except ValueError:
            height = 0
        page = (height if height > 1 else 25) - 1
        shown = page
        sys.stdout.write('\n'.join(lines[:page]) + '\n')
        while shown < len(lines):
            sys.stdout.write('-- more --')
            sys.stdout.flush()
            key = read_key()
            if key in ('q', 'Q'):
                sys.stdout.write('\r          \r')
                break
            if key in ('\r', '\n'):
                sys.stdout.write('\r          \r' + lines[shown] + '\n')
                shown += 1
                continue
            if key in ('b', 'B', '\x1b'):
                shown = max(0, shown - 2 * page)
            sys.stdout.write('\n' + '\n'.join(lines[shown:shown + page]) + '\n')
            shown += page
    finally:
        if saved is not None:
            termios.tcsetattr(fd, termios.TCSAFLUSH, saved)


ttypager = tty_pager


def get_pager():
    """Choose how to page text: $MANPAGER/$PAGER, less, or the built-in
    pager on a terminal; plain output otherwise."""
    stdin, stdout = sys.stdin, sys.stdout
    if (stdin is None or not hasattr(stdin, 'isatty') or not hasattr(stdout, 'isatty')
            or not stdin.isatty() or not stdout.isatty()
            or sys.platform == 'emscripten'):
        return plain_pager
    term = os.environ.get('TERM')
    command = os.environ.get('MANPAGER') or os.environ.get('PAGER')
    if command:
        if term in ('dumb', 'emacs'):
            return lambda text, title='': pipe_pager(plain(text), command, title)
        return lambda text, title='': pipe_pager(text, command, title)
    if term in ('dumb', 'emacs'):
        return plain_pager
    import shutil
    if shutil.which('less'):
        return lambda text, title='': pipe_pager(text, 'less', title)
    return tty_pager


getpager = get_pager


def pager(text, title=''):
    """Page *text* (the pager is chosen on first use)."""
    global pager
    pager = get_pager()
    pager(text, title)


# ------------------------------------------------- rendering entry points

text = TextDoc()
plaintext = _PlainTextDoc()
html = HTMLDoc()


def describe(thing):
    """Short description of *thing*, e.g. 'built-in function len'."""
    if inspect.ismodule(thing):
        if thing.__name__ in sys.builtin_module_names:
            return 'built-in module ' + thing.__name__
        if hasattr(thing, '__path__'):
            return 'package ' + thing.__name__
        return 'module ' + thing.__name__
    if inspect.isbuiltin(thing):
        return 'built-in function ' + thing.__name__
    if inspect.isgetsetdescriptor(thing) or inspect.ismemberdescriptor(thing):
        kind = 'getset' if inspect.isgetsetdescriptor(thing) else 'member'
        owner = thing.__objclass__
        return f'{kind} descriptor {owner.__module__}.{owner.__name__}.{thing.__name__}'
    if inspect.isclass(thing):
        return 'class ' + thing.__name__
    if inspect.isfunction(thing):
        return 'function ' + thing.__name__
    if inspect.ismethod(thing):
        return 'method ' + thing.__name__
    if inspect.ismethodwrapper(thing):
        return 'method wrapper ' + thing.__name__
    if inspect.ismethoddescriptor(thing):
        name = getattr(thing, '__name__', None)
        if name is not None:
            return 'method descriptor ' + name
    return type(thing).__name__


_MISSING_DOC = '''\
No Python documentation found for %r.
Use help() to get the interactive help utility.
Use help(str) for help on the str class.'''


def resolve(thing, forceload=0):
    """Return (object, name) for an object or a dotted name; raise
    ImportError when the name cannot be found."""
    if isinstance(thing, str):
        found = locate(thing, forceload)
        if found is None:
            raise ImportError(_MISSING_DOC % thing)
        return found, thing
    name = getattr(thing, '__name__', None)
    return thing, (name if isinstance(name, str) else None)


def render_doc(thing, title='Python Library Documentation: %s', forceload=0,
               renderer=None):
    """Documentation of *thing* (object or dotted name) as text."""
    renderer = renderer or text
    obj, name = resolve(thing, forceload)
    desc = describe(obj)
    module = inspect.getmodule(obj)
    if name and '.' in name:
        desc += ' in ' + name.rpartition('.')[0]
    elif module and module is not obj:
        desc += ' in module ' + module.__name__
    documentable = (inspect.ismodule(obj) or inspect.isclass(obj)
                    or inspect.isroutine(obj) or inspect.isdatadescriptor(obj)
                    or _getdoc(obj))
    if not documentable:
        # an instance: show its class (or the origin of a generic alias)
        if hasattr(obj, '__origin__'):
            obj = obj.__origin__
        else:
            obj = type(obj)
            desc += ' object'
    return title % desc + '\n\n' + renderer.document(obj, name)


def _topic_title(thing):
    """What help(thing) shows in the pager title."""
    if isinstance(thing, str):
        return thing
    for attr in ('__qualname__', '__name__'):
        value = getattr(thing, attr, None)
        if isinstance(value, str):
            return value
    return type(thing).__name__ + ' object'


def doc(thing, title='Python Library Documentation: %s', forceload=0,
        output=None, is_cli=False):
    """Show the documentation of *thing* in the pager, or write it to
    *output*."""
    if output is not None:
        try:
            page = render_doc(thing, title, forceload, plaintext)
        except ImportError as exc:
            page = str(exc)
        output.write(page)
        return
    try:
        page = render_doc(thing, title, forceload)
    except ImportError as exc:
        if is_cli:
            raise
        print(exc)
        return
    pager(page, f'Help on {_topic_title(thing)}')


def writedoc(thing, forceload=0):
    """Write the HTML documentation of *thing* to NAME.html."""
    obj, name = resolve(thing, forceload)
    page = html.page(describe(obj), html.document(obj, name))
    with open(name + '.html', 'w', encoding='utf-8',
              errors='xmlcharrefreplace') as file:
        file.write(page)
    print('wrote', name + '.html')


def writedocs(dir, pkgpath='', done=None):
    """Write HTML documentation for every module below *dir*."""
    for _, modname, _ in pkgutil.walk_packages([dir], pkgpath):
        writedoc(modname)


# ------------------------------------------------------- interactive help

class Helper:
    """The interactive help utility (the builtin help() object)."""

    # keyword/topic -> topic label in pydoc_data.topics plus related
    # topics, or the name of another entry
    keywords = {
        'False': '',
        'None': '',
        'True': '',
        'and': 'BOOLEAN',
        'as': 'with',
        'assert': ('assert', ''),
        'async': ('async', ''),
        'await': ('await', ''),
        'break': ('break', 'while for'),
        'case': ('match', ''),
        'class': ('class', 'CLASSES SPECIALMETHODS'),
        'continue': ('continue', 'while for'),
        'def': ('function', ''),
        'del': ('del', 'BASICMETHODS'),
        'elif': 'if',
        'else': ('while', 'for if try'),
        'except': 'try',
        'finally': 'try',
        'for': ('for', 'break continue while'),
        'from': 'import',
        'global': ('global', 'nonlocal NAMESPACES'),
        'if': ('if', 'TRUTHVALUE'),
        'import': ('import', 'MODULES'),
        'in': ('in', 'SEQUENCEMETHODS'),
        'is': 'COMPARISON',
        'lambda': ('lambda', 'FUNCTIONS'),
        'lazy': ('lazy', 'import'),
        'match': ('match', ''),
        'nonlocal': ('nonlocal', 'global NAMESPACES'),
        'not': 'BOOLEAN',
        'or': 'BOOLEAN',
        'pass': ('pass', ''),
        'raise': ('raise', 'EXCEPTIONS'),
        'return': ('return', 'FUNCTIONS'),
        'try': ('try', 'EXCEPTIONS'),
        'while': ('while', 'break continue if TRUTHVALUE'),
        'with': ('with', 'CONTEXTMANAGERS EXCEPTIONS yield'),
        'yield': ('yield', ''),
    }

    symbols = {
        '%': 'OPERATORS FORMATTING',
        '**': 'POWER',
        ',': 'TUPLES LISTS FUNCTIONS',
        '.': 'ATTRIBUTES FLOAT MODULES OBJECTS',
        '...': 'ELLIPSIS',
        ':': 'SLICINGS DICTIONARYLITERALS',
        '@': 'def class',
        '\\': 'STRINGS',
        ':=': 'ASSIGNMENTEXPRESSIONS',
        '_': 'PRIVATENAMES',
        '__': 'PRIVATENAMES SPECIALMETHODS',
        '`': 'BACKQUOTES',
        '(': 'TUPLES FUNCTIONS CALLS',
        ')': 'TUPLES FUNCTIONS CALLS',
        '[': 'LISTS SUBSCRIPTS SLICINGS',
        ']': 'LISTS SUBSCRIPTS SLICINGS',
    }

    topics = {
        'TYPES': ('types', 'STRINGS UNICODE NUMBERS SEQUENCES MAPPINGS '
                  'FUNCTIONS CLASSES MODULES FILES inspect'),
        'STRINGS': ('strings', 'str UNICODE SEQUENCES STRINGMETHODS FORMATTING TYPES'),
        'STRINGMETHODS': ('string-methods', 'STRINGS FORMATTING'),
        'FORMATTING': ('formatstrings', 'OPERATORS'),
        'UNICODE': ('strings', 'encodings unicode SEQUENCES STRINGMETHODS FORMATTING TYPES'),
        'NUMBERS': ('numbers', 'INTEGER FLOAT COMPLEX TYPES'),
        'INTEGER': ('integers', 'int range'),
        'FLOAT': ('floating', 'float math'),
        'COMPLEX': ('imaginary', 'complex cmath'),
        'SEQUENCES': ('typesseq', 'STRINGMETHODS FORMATTING range LISTS'),
        'MAPPINGS': 'DICTIONARIES',
        'FUNCTIONS': ('typesfunctions', 'def TYPES'),
        'METHODS': ('typesmethods', 'class def CLASSES TYPES'),
        'CODEOBJECTS': ('objects', 'compile FUNCTIONS TYPES'),
        'TYPEOBJECTS': ('objects', 'types TYPES'),
        'FRAMEOBJECTS': 'TYPES',
        'TRACEBACKS': 'TYPES',
        'NONE': ('objects', 'None'),
        'ELLIPSIS': ('objects', 'SLICINGS'),
        'SPECIALATTRIBUTES': ('specialattrs', ''),
        'CLASSES': ('types', 'class SPECIALMETHODS PRIVATENAMES'),
        'MODULES': ('typesmodules', 'import'),
        'PACKAGES': 'import',
        'EXPRESSIONS': ('operator-summary', 'lambda or and not in is BOOLEAN '
                        'COMPARISON BITWISE SHIFTING BINARY FORMATTING POWER '
                        'UNARY ATTRIBUTES SUBSCRIPTS SLICINGS CALLS TUPLES '
                        'LISTS DICTIONARIES'),
        'OPERATORS': 'EXPRESSIONS',
        'PRECEDENCE': 'EXPRESSIONS',
        'OBJECTS': ('objects', 'TYPES'),
        'SPECIALMETHODS': ('specialnames', 'BASICMETHODS ATTRIBUTEMETHODS '
                           'CALLABLEMETHODS SEQUENCEMETHODS MAPPINGMETHODS '
                           'NUMBERMETHODS CLASSES'),
        'BASICMETHODS': ('customization', 'hash repr str SPECIALMETHODS'),
        'ATTRIBUTEMETHODS': ('attribute-references', 'ATTRIBUTES SPECIALMETHODS'),
        'CALLABLEMETHODS': ('callable-types', 'CALLS SPECIALMETHODS'),
        'SEQUENCEMETHODS': ('sequence-types', 'SEQUENCES SPECIALMETHODS'),
        'MAPPINGMETHODS': ('sequence-types', 'MAPPINGS SPECIALMETHODS'),
        'NUMBERMETHODS': ('numeric-types', 'NUMBERS AUGMENTEDASSIGNMENT SPECIALMETHODS'),
        'EXECUTION': ('execmodel', 'NAMESPACES DYNAMICFEATURES EXCEPTIONS'),
        'NAMESPACES': ('naming', 'global nonlocal ASSIGNMENT DELETION DYNAMICFEATURES'),
        'DYNAMICFEATURES': ('execmodel', ''),
        'SCOPING': 'NAMESPACES',
        'FRAMES': 'NAMESPACES',
        'EXCEPTIONS': ('exceptions', 'try except finally raise'),
        'CONVERSIONS': ('conversions', ''),
        'IDENTIFIERS': ('identifiers', 'keywords SPECIALIDENTIFIERS'),
        'SPECIALIDENTIFIERS': ('id-classes', ''),
        'PRIVATENAMES': ('atom-identifiers', ''),
        'LITERALS': ('atom-literals', 'STRINGS NUMBERS TUPLELITERALS '
                     'LISTLITERALS DICTIONARYLITERALS'),
        'TUPLES': 'SEQUENCES',
        'TUPLELITERALS': ('exprlists', 'TUPLES LITERALS'),
        'LISTS': ('typesseq-mutable', 'LISTLITERALS'),
        'LISTLITERALS': ('lists', 'LISTS LITERALS'),
        'DICTIONARIES': ('typesmapping', 'DICTIONARYLITERALS'),
        'DICTIONARYLITERALS': ('dict', 'DICTIONARIES LITERALS'),
        'ATTRIBUTES': ('attribute-references', 'getattr hasattr setattr ATTRIBUTEMETHODS'),
        'SUBSCRIPTS': ('subscriptions', 'SEQUENCEMETHODS'),
        'SLICINGS': ('slicings', 'SEQUENCEMETHODS'),
        'CALLS': ('calls', 'EXPRESSIONS'),
        'POWER': ('power', 'EXPRESSIONS'),
        'UNARY': ('unary', 'EXPRESSIONS'),
        'BINARY': ('binary', 'EXPRESSIONS'),
        'SHIFTING': ('shifting', 'EXPRESSIONS'),
        'BITWISE': ('bitwise', 'EXPRESSIONS'),
        'COMPARISON': ('comparisons', 'EXPRESSIONS BASICMETHODS'),
        'BOOLEAN': ('booleans', 'EXPRESSIONS TRUTHVALUE'),
        'ASSERTION': 'assert',
        'ASSIGNMENT': ('assignment', 'AUGMENTEDASSIGNMENT'),
        'AUGMENTEDASSIGNMENT': ('augassign', 'NUMBERMETHODS'),
        'ASSIGNMENTEXPRESSIONS': ('assignment', ''),
        'DELETION': 'del',
        'RETURNING': 'return',
        'IMPORTING': 'import',
        'CONDITIONAL': ('conditional', 'if'),
        'LOOPING': ('compound', 'for while break continue'),
        'TRUTHVALUE': ('truth', 'if while and or not BASICMETHODS'),
        'CONTEXTMANAGERS': ('context-managers', 'with'),
    }

    _CONSTANTS = {'True': True, 'False': False, 'None': None}

    def __init__(self, input=None, output=None):
        self._input = input
        self._output = output

    @property
    def input(self):
        return self._input or sys.stdin

    @property
    def output(self):
        return self._output or sys.stdout

    def __repr__(self):
        if inspect.stack()[1][3] == '?':
            self()
            return ''
        return f'<{type(self).__module__}.{type(self).__qualname__} instance>'

    _GoInteractive = object()

    def __call__(self, request=_GoInteractive):
        if request is not self._GoInteractive:
            try:
                self.help(request)
            except KeyboardInterrupt:
                self.output.write('\n')
            return
        self.intro()
        self.interact()
        self.output.write('''
Leaving the help utility and returning to the Python interpreter.
At the Python prompt, help(object) shows the documentation of an
object, and help('text') looks the text up just as if it had been
typed at the help> prompt.
''')

    def interact(self):
        """Read requests at the help> prompt until 'q'/'quit'/'exit'."""
        self.output.write('\n')
        while True:
            try:
                request = self.getline('help> ')
            except (KeyboardInterrupt, EOFError):
                break
            if request is None:
                break
            request = request.strip()
            if not request:
                continue
            quoted = (len(request) > 2 and request[0] == request[-1]
                      and request[0] in ('"', "'")
                      and request[0] not in request[1:-1])
            if quoted:
                request = request[1:-1]
            if request.lower() in ('q', 'quit', 'exit'):
                break
            if request == 'help':
                self.intro()
            else:
                self.help(request)

    def getline(self, prompt):
        """Read a line of input after showing *prompt*."""
        if self.input is sys.stdin:
            return input(prompt)
        self.output.write(prompt)
        self.output.flush()
        return self.input.readline()

    def help(self, request, is_cli=False):
        """Show help for *request*: an object, or a name, keyword, symbol
        or topic given as a string."""
        def show(thing):
            doc(thing, 'Help on %s:', output=self._output, is_cli=is_cli)

        if isinstance(request, Helper):
            self()
        elif not isinstance(request, str):
            show(request)
        else:
            request = request.strip()
            if request == 'keywords':
                self.listkeywords()
            elif request == 'symbols':
                self.listsymbols()
            elif request == 'topics':
                self.listtopics()
            elif request == 'modules':
                self.listmodules()
            elif request.startswith('modules '):
                self.listmodules(request.split()[1])
            elif request in self.symbols:
                self.showsymbol(request)
            elif request in self._CONSTANTS:
                show(self._CONSTANTS[request])
            elif request in self.keywords or request in self.topics:
                self.showtopic(request)
            else:
                show(request or str)
        self.output.write('\n')

    def intro(self):
        version = '%d.%d' % sys.version_info[:2]
        self.output.write(f'''\
Welcome to the help utility of Python {version}.  The tutorial is at
https://docs.python.org/{version}/tutorial/.

Type the name of a module, keyword or topic to read its documentation.
"modules", "keywords", "symbols" and "topics" list what is available;
"modules spam" lists the modules whose name or summary mentions "spam".

Type "q", "quit" or "exit" to leave the help utility.
''')

    def list(self, items, columns=4, width=80):
        """Write *items* sorted, in columns."""
        items = sorted(items)
        column_width = width // columns
        rows = (len(items) + columns - 1) // columns
        for row in range(rows):
            cells = []
            for column in range(columns):
                index = column * rows + row
                if index < len(items):
                    cells.append(items[index])
            line = ''.join(cell.ljust(column_width) for cell in cells[:-1])
            self.output.write(line + (cells[-1] if cells else '') + '\n')

    def listkeywords(self):
        self.output.write('\nHere is a list of the Python keywords.  '
                          'Enter any keyword to get more help.\n\n')
        self.list(self.keywords.keys())

    def listsymbols(self):
        self.output.write('\nHere is a list of the punctuation symbols which '
                          'Python assigns special meaning\nto. Enter any '
                          'symbol to get more help.\n\n')
        self.list(self.symbols.keys())

    def listtopics(self):
        self.output.write('\nHere is a list of available topics.  '
                          'Enter any topic name to get more help.\n\n')
        self.list(self.topics.keys(), columns=3)

    def _lookup_topic(self, topic, more_xrefs=''):
        """(text, related topics) for a topic or keyword; raise KeyError
        if there is none."""
        import pydoc_data.topics
        target = self.topics.get(topic, self.keywords.get(topic))
        seen = set()
        while isinstance(target, str) and target and target not in seen:
            seen.add(target)
            target = self.topics.get(target, self.keywords.get(target))
        if not target or isinstance(target, str):
            raise KeyError(topic)
        label, xrefs = target
        text = pydoc_data.topics.topics[label]
        if more_xrefs:
            xrefs = ((xrefs or '') + ' ' + more_xrefs).strip()
        return text, xrefs

    def _resolve_alias(self, topic):
        target = self.topics.get(topic, self.keywords.get(topic))
        if isinstance(target, str) and target:
            return self._resolve_alias(target)
        return topic

    def showtopic(self, topic, more_xrefs=''):
        """Show the help text of a topic or keyword."""
        try:
            text, xrefs = self._lookup_topic(topic, more_xrefs)
        except ImportError:
            self.output.write('\nTopic and keyword help is unavailable: '
                              'the pydoc_data.topics module is missing.\n')
            return
        except KeyError:
            self.output.write(f'no documentation found for {topic!r}\n')
            return
        page = text.strip() + '\n'
        if xrefs:
            related = 'Related help topics: ' + ', '.join(xrefs.split()) + '\n'
            page += '\n%s\n' % '\n'.join(textwrap.wrap(related, 72))
        if self._output is None:
            pager(page, f'Help on {self._resolve_alias(topic)}')
        else:
            self.output.write(page)

    def _gettopic(self, topic, more_xrefs=''):
        """(text, related topics) of a topic, for the HTML server."""
        try:
            return self._lookup_topic(topic, more_xrefs)
        except ImportError:
            return ('Topic and keyword help is unavailable: the '
                    'pydoc_data.topics module is missing.\n', '')
        except KeyError:
            raise ValueError('could not find topic')

    def showsymbol(self, symbol):
        topic, _, xrefs = self.symbols[symbol].partition(' ')
        self.showtopic(topic, xrefs)

    def listmodules(self, key=''):
        if key:
            self.output.write(f"\nHere is a list of modules whose name or "
                              f"summary contains '{key}'.\nIf there are any, "
                              f"enter a module name to get more help.\n\n")
            apropos(key)
            return
        self.output.write('\nPlease wait a moment while I gather a list of '
                          'all available modules...\n\n')
        names = set()

        def collect(path, modname, desc):
            if modname and modname.endswith('.__init__'):
                modname = modname[:-9] + ' (package)'
            if '.' not in modname:
                names.add(modname)

        ModuleScanner().run(collect, onerror=lambda modname: collect(None, modname, None))
        self.list(names)
        self.output.write('\nEnter any module name to get more help.  Or, '
                          'type "modules spam" to search\nfor modules whose '
                          'name or summary contain the string "spam".\n')


help = Helper()


class ModuleScanner:
    """Walks all importable modules, calling back for those whose name or
    one-line summary contains a key (all modules without a key)."""

    def run(self, callback, key=None, completer=None, onerror=None):
        key = key.lower() if key else key
        self.quit = False
        for modname in sys.builtin_module_names:
            if modname == '__main__':
                continue
            if key is None:
                callback(None, modname, '')
                continue
            desc = (__import__(modname).__doc__ or '').split('\n')[0]
            if key in (modname + ' - ' + desc).lower():
                callback(None, modname, desc)

        for finder, modname, ispkg in pkgutil.walk_packages(onerror=onerror):
            if self.quit:
                break
            if key is None:
                callback(None, modname, '')
                continue
            found = self._describe(finder, modname, onerror)
            if found is None:
                continue
            path, desc = found
            if key in (modname + ' - ' + desc).lower():
                callback(path, modname, desc)
        if completer:
            completer()

    @staticmethod
    def _describe(finder, modname, onerror):
        """(path, summary line) of a module without importing it when
        possible; None if it cannot be examined."""
        try:
            spec = finder.find_spec(modname)
        except SyntaxError:
            return None
        if spec is None:
            return None
        loader = spec.loader
        if hasattr(loader, 'get_source'):
            try:
                source = loader.get_source(modname)
            except Exception:
                if onerror:
                    onerror(modname)
                return None
            desc = source_synopsis(io.StringIO(source or '')) or ''
            path = loader.get_filename(modname) if hasattr(loader, 'get_filename') else None
            return path, desc
        try:
            module = importlib._bootstrap._load(spec)
        except ImportError:
            if onerror:
                onerror(modname)
            return None
        text = module.__doc__
        return getattr(module, '__file__', None), (text.splitlines()[0] if text else '')


def apropos(key):
    """Print the modules whose name or summary line contains *key*."""
    def report(path, modname, desc):
        if modname.endswith('.__init__'):
            modname = modname[:-9] + ' (package)'
        print(modname, desc and '- ' + desc)

    with warnings.catch_warnings():
        warnings.filterwarnings('ignore')
        ModuleScanner().run(report, key, onerror=lambda modname: None)


# ------------------------------------------------------- HTTP server

def _start_server(urlhandler, hostname, port):
    """Serve pages produced by urlhandler(path, content_type) in a thread.

    Returns the thread once it serves (or failed); it has the attributes
    serving, url, docserver, error and a stop() method.
    """
    import email.message
    import http.server
    import select
    import threading

    class Handler(http.server.BaseHTTPRequestHandler):
        MessageClass = email.message.Message

        def do_GET(self):
            content_type = 'text/css' if self.path.endswith('.css') else 'text/html'
            body = self.server.urlhandler(self.path, content_type).encode('utf-8')
            self.send_response(200)
            self.send_header('Content-Type', f'{content_type}; charset=UTF-8')
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    class Server(http.server.HTTPServer):
        def __init__(self, address, urlhandler, on_ready):
            self.urlhandler = urlhandler
            self.on_ready = on_ready
            self.quit = False
            super().__init__(address, Handler)

        def server_activate(self):
            super().server_activate()
            self.on_ready(self)

        def serve_until_quit(self):
            while not self.quit:
                ready, _, _ = select.select([self.socket.fileno()], [], [], 1)
                if ready:
                    self.handle_request()
            self.server_close()

    class ServerThread(threading.Thread):
        def __init__(self):
            super().__init__(daemon=True)
            self.host = hostname
            self.port = int(port)
            self.serving = False
            self.error = None
            self.docserver = None
            self.url = None

        def run(self):
            try:
                server = Server((self.host, self.port), urlhandler, self._ready)
                self.docserver = server
                server.serve_until_quit()
            except Exception as exc:
                self.error = exc

        def _ready(self, server):
            self.port = server.server_port
            self.url = f'http://{self.host}:{self.port}/'
            self.serving = True

        def stop(self):
            self.docserver.quit = True
            self.join()
            self.docserver = None
            self.serving = False
            self.url = None

    thread = ServerThread()
    thread.start()
    while not thread.error and not (thread.serving and thread.docserver):
        time.sleep(0.01)
    return thread


def _url_handler(url, content_type="text/html"):
    """Produce the page for *url* of the documentation server (HTML, or
    the style sheet for content_type 'text/css')."""

    class _PageDoc(HTMLDoc):
        def page(self, title, contents):
            css = '<link rel="stylesheet" type="text/css" href="pydoc_data/_pydoc.css">'
            return (f'<!DOCTYPE html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
                    f'<title>Pydoc: {title}</title>\n{css}</head><body>'
                    f'{navbar()}<div style="clear:both;padding-top:.5em;">'
                    f'{contents}</div>\n</body></html>')

    page_doc = _PageDoc()

    def navbar():
        version = page_doc.escape('%s [%s, %s]' % (
            platform.python_version(), platform.python_build()[0],
            platform.python_compiler()))
        system = page_doc.escape(platform.platform(terse=True))
        return (f'\n<div style="float:left">Python {version}<br>{system}</div>\n'
                '<div style="float:right"><div style="text-align:center">'
                '<a href="index.html">Module Index</a> : '
                '<a href="topics.html">Topics</a> : '
                '<a href="keywords.html">Keywords</a></div>\n'
                '<div><form action="get" style="display:inline;">'
                '<input type=text name=key size=15>'
                '<input type=submit value="Get"></form>&nbsp;'
                '<form action="search" style="display:inline;">'
                '<input type=text name=key size=15>'
                '<input type=submit value="Search"></form></div></div>\n')

    def link(name, prefix=''):
        return f'<a href="{prefix}{name}{"" if prefix else ".html"}">{name}</a>'

    def index_page():
        builtin_names = [name for name in sys.builtin_module_names if name != '__main__']
        parts = [page_doc.heading('<strong class="title">Index of Modules</strong>'),
                 '<p>' + page_doc.bigsection('Built-in Modules', 'index',
                                             page_doc.multicolumn(builtin_names, link))]
        seen = {}
        for directory in sys.path:
            parts.append(page_doc.index(directory, seen))
        return 'Index of Modules', ''.join(parts)

    def search_page(key):
        found = []

        def report(path, modname, desc):
            if modname.endswith('.__init__'):
                modname = modname[:-9] + ' (package)'
            found.append(link(modname) + (desc and '- ' + desc))

        with warnings.catch_warnings():
            warnings.filterwarnings('ignore')
            ModuleScanner().run(report, key, onerror=lambda modname: None)
        body = (page_doc.heading('<strong class="title">Search Results</strong>')
                + page_doc.bigsection(f'key = {key}', 'index', '<br>'.join(found)))
        return 'Search Results', body

    def listing_page(title, names):
        body = (page_doc.heading('<strong class="title">INDEX</strong>')
                + page_doc.bigsection(title, 'index', page_doc.multicolumn(
                    sorted(names), lambda name: link(name, 'topic?key='))))
        return title, body

    def topic_page(topic):
        helper = Helper(io.StringIO(), io.StringIO())
        text_, xrefs = helper._gettopic(topic)
        kind = 'KEYWORD' if topic in helper.keywords else 'TOPIC'
        parts = [page_doc.heading(f'<strong class="title">{kind}</strong>'),
                 page_doc.bigsection(topic, 'index',
                                     f'<pre>{page_doc.markup(text_)}</pre>')]
        if xrefs:
            parts.append(page_doc.section(
                'Related help topics: ', 'index',
                page_doc.multicolumn(sorted(xrefs.split()),
                                     lambda name: link(name, 'topic?key='))))
        return f'{kind} {topic}', ''.join(parts)

    def object_page(name):
        obj = locate(name, forceload=1)
        if obj is None and name != 'None':
            raise ValueError('could not find object')
        return describe(obj), page_doc.document(obj, name)

    def error_page(requested, exc):
        details = '<br>'.join(page_doc.escape(line)
                              for line in format_exception_only(type(exc), exc))
        body = (page_doc.heading('<strong class="title">Error</strong>')
                + page_doc.bigsection(requested, 'error', details))
        return f'Error - {requested}', body

    def html_page(path):
        requested = path
        if path.endswith('.html'):
            path = path[:-5]
        try:
            if path in ('', 'index'):
                title, body = index_page()
            elif path == 'topics':
                title, body = listing_page('Topics', Helper.topics)
            elif path == 'keywords':
                title, body = listing_page('Keywords', Helper.keywords)
            elif '=' in path:
                op, _, arg = path.partition('=')
                if op == 'search?key':
                    title, body = search_page(arg)
                elif op == 'topic?key':
                    try:
                        title, body = topic_page(arg)
                    except ValueError:
                        title, body = object_page(arg)
                elif op == 'get?key':
                    if arg in ('', 'index'):
                        title, body = index_page()
                    else:
                        try:
                            title, body = object_page(arg)
                        except ValueError:
                            title, body = topic_page(arg)
                else:
                    raise ValueError('bad pydoc url')
            else:
                title, body = object_page(path)
        except Exception as exc:
            title, body = error_page(requested, exc)
        return page_doc.page(title, body)

    url = url[1:] if url.startswith('/') else url
    if content_type == 'text/css':
        here = os.path.dirname(os.path.realpath(__file__))
        with open(os.path.join(here, url)) as css:
            return css.read()
    if content_type == 'text/html':
        return html_page(url)
    raise TypeError(f'unknown content type {content_type!r} for url {url}')


def browse(port=0, *, open_browser=True, hostname='localhost'):
    """Run the documentation server (until 'q'), optionally opening a
    web browser on it."""
    import webbrowser
    server = _start_server(_url_handler, hostname, port)
    if server.error:
        print(server.error)
        return
    commands = 'Server commands: [b]rowser, [q]uit'
    if open_browser:
        webbrowser.open(server.url)
    try:
        print('Server ready at', server.url)
        print(commands)
        while server.serving:
            command = input('server> ').lower()
            if command == 'q':
                break
            if command == 'b':
                webbrowser.open(server.url)
            else:
                print(commands)
    except (KeyboardInterrupt, EOFError):
        print()
    finally:
        if server.serving:
            server.stop()
            print('Server stopped')


# ------------------------------------------------------- command line

def ispath(x):
    return isinstance(x, str) and os.sep in x


def _get_revised_path(given_path, argv0):
    """sys.path for the command line tool: the current directory is added
    and the script's directory removed (unless it is pydoc's own).  None
    if the current directory is already present."""
    if '' in given_path or os.curdir in given_path or os.getcwd() in given_path:
        return None
    revised = list(given_path)
    script_dir = os.path.dirname(argv0)
    if script_dir in revised and not os.path.samefile(script_dir, os.path.dirname(__file__)):
        revised.remove(script_dir)
    revised.insert(0, os.getcwd())
    return revised


def _adjust_cli_sys_path():
    revised = _get_revised_path(sys.path, sys.argv[0])
    if revised is not None:
        sys.path[:] = revised


_USAGE = """\
pydoc - show Python documentation

{cmd} NAME ...
    Show the documentation of NAME: a keyword, topic, module, package or a
    dotted name of an object inside a module.  A NAME containing '{sep}' is
    the path of a Python source file.  'keywords', 'topics', 'symbols' and
    'modules' list what is available.

{cmd} -k WORD
    List the modules whose one-line summary contains WORD.

{cmd} -n HOST
    Start the documentation server on HOST (default: localhost).

{cmd} -p PORT
    Start the documentation server on PORT (0 picks a free port).

{cmd} -b
    Start the server on a free port and open a web browser; combine with
    -n and -p to choose the host and port.

{cmd} -w NAME ...
    Write the HTML documentation of NAME to NAME.html in the current
    directory (for a directory: of every module in it).
"""


def cli():
    """Command line interface; see the module docstring."""
    import getopt
    _adjust_cli_sys_path()
    try:
        opts, args = getopt.getopt(sys.argv[1:], 'bk:n:p:w')
    except getopt.error:
        opts, args = None, None
    if opts is None or (not args and not opts):
        cmd = os.path.splitext(os.path.basename(sys.argv[0]))[0]
        print(_USAGE.format(cmd=cmd, sep=os.sep))
        return
    writing = serve = open_browser = False
    port, hostname = 0, 'localhost'
    for opt, value in opts:
        if opt == '-b':
            serve = open_browser = True
        elif opt == '-k':
            apropos(value)
            return
        elif opt == '-p':
            serve, port = True, value
        elif opt == '-w':
            writing = True
        elif opt == '-n':
            serve, hostname = True, value
    if serve:
        browse(port, hostname=hostname, open_browser=open_browser)
        return
    if not args:
        cmd = os.path.splitext(os.path.basename(sys.argv[0]))[0]
        print(_USAGE.format(cmd=cmd, sep=os.sep))
        return
    for arg in args:
        if ispath(arg) and not os.path.exists(arg):
            print(f'file {arg!r} does not exist')
            sys.exit(1)
        try:
            if ispath(arg) and os.path.isfile(arg):
                arg = importfile(arg)
            if not writing:
                help.help(arg, is_cli=True)
            elif ispath(arg) and os.path.isdir(arg):
                writedocs(arg)
            else:
                writedoc(arg)
        except (ImportError, ErrorDuringImport) as exc:
            print(exc)
            sys.exit(1)


if __name__ == '__main__':
    cli()
