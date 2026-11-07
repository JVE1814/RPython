"""Get information about live objects.

This module answers questions about modules, classes, functions,
methods, frames, tracebacks and code objects:

* type checks such as isfunction(), isclass(), iscoroutinefunction();
* source retrieval: getsource(), getsourcelines(), getfile(), getcomments();
* class and member inspection: getmembers(), getmro(), classify_class_attrs(),
  getattr_static();
* call signatures: signature(), Signature, Parameter, BoundArguments;
* interpreter stack: stack(), trace(), getframeinfo(), currentframe();
* generator / coroutine state: getgeneratorstate(), getcoroutinelocals()...
"""

__all__ = [
    "AGEN_CLOSED", "AGEN_CREATED", "AGEN_RUNNING", "AGEN_SUSPENDED",
    "ArgInfo", "Arguments", "Attribute", "BlockFinder", "BoundArguments", "BufferFlags",
    "CORO_CLOSED", "CORO_CREATED", "CORO_RUNNING", "CORO_SUSPENDED",
    "CO_ASYNC_GENERATOR", "CO_COROUTINE", "CO_GENERATOR",
    "CO_ITERABLE_COROUTINE", "CO_NESTED", "CO_NEWLOCALS", "CO_NOFREE",
    "CO_OPTIMIZED", "CO_VARARGS", "CO_VARKEYWORDS", "CO_HAS_DOCSTRING",
    "CO_METHOD", "ClassFoundException", "ClosureVars", "EndOfBlock",
    "FrameInfo", "FullArgSpec", "GEN_CLOSED", "GEN_CREATED", "GEN_RUNNING",
    "GEN_SUSPENDED", "Parameter", "Signature", "TPFLAGS_IS_ABSTRACT",
    "Traceback", "classify_class_attrs", "cleandoc", "currentframe",
    "findsource", "formatannotation", "formatannotationrelativeto",
    "formatargvalues", "get_annotations", "getabsfile", "getargs",
    "getargvalues", "getasyncgenlocals", "getasyncgenstate", "getattr_static",
    "getblock", "getcallargs", "getclasstree", "getclosurevars",
    "getcomments", "getcoroutinelocals", "getcoroutinestate", "getdoc",
    "getfile", "getframeinfo", "getfullargspec", "getgeneratorlocals",
    "getgeneratorstate", "getinnerframes", "getlineno", "getmembers",
    "getmembers_static", "getmodule", "getmodulename", "getmro",
    "getouterframes", "getsource", "getsourcefile", "getsourcelines",
    "indentsize", "isabstract", "isasyncgen", "isasyncgenfunction",
    "isawaitable", "isbuiltin", "isclass", "iscode", "iscoroutine",
    "iscoroutinefunction", "isdatadescriptor", "isframe", "isfunction",
    "isgenerator", "isgeneratorfunction", "isgetsetdescriptor",
    "ismemberdescriptor", "ismethod", "ismethoddescriptor",
    "ismethodwrapper", "ismodule", "ispackage", "isroutine", "istraceback",
    "markcoroutinefunction", "signature", "stack", "trace", "unwrap",
    "walktree",
]

import abc
import builtins
import collections
import collections.abc
import enum
import functools
import importlib.machinery
import itertools
import linecache
import os
import sys
import types
import weakref
from keyword import iskeyword
from operator import attrgetter
from annotationlib import Format, ForwardRef
from annotationlib import get_annotations  # re-exported


# Code object flags (see Include/cpython/code.h).
CO_OPTIMIZED = 0x0001
CO_NEWLOCALS = 0x0002
CO_VARARGS = 0x0004
CO_VARKEYWORDS = 0x0008
CO_NESTED = 0x0010
CO_GENERATOR = 0x0020
CO_NOFREE = 0x0040
CO_COROUTINE = 0x0080
CO_ITERABLE_COROUTINE = 0x0100
CO_ASYNC_GENERATOR = 0x0200
CO_HAS_DOCSTRING = 0x4000000
CO_METHOD = 0x8000000

# Type flag set on classes with abstract methods.
TPFLAGS_IS_ABSTRACT = 1 << 20


class BufferFlags(enum.IntFlag):
    """Flags for the buffer protocol (PEP 688)."""
    SIMPLE = 0x0
    WRITABLE = 0x1
    FORMAT = 0x4
    ND = 0x8
    STRIDES = 0x10 | ND
    C_CONTIGUOUS = 0x20 | STRIDES
    F_CONTIGUOUS = 0x40 | STRIDES
    ANY_CONTIGUOUS = 0x80 | STRIDES
    INDIRECT = 0x100 | STRIDES
    CONTIG = ND | WRITABLE
    CONTIG_RO = ND
    STRIDED = STRIDES | WRITABLE
    STRIDED_RO = STRIDES
    RECORDS = STRIDES | WRITABLE | FORMAT
    RECORDS_RO = STRIDES | FORMAT
    FULL = INDIRECT | WRITABLE | FORMAT
    FULL_RO = INDIRECT | FORMAT
    READ = 0x100
    WRITE = 0x200


# --------------------------------------------------------------------------
# Type predicates

def ismodule(object):
    """True for module objects."""
    return isinstance(object, types.ModuleType)

def isclass(object):
    """True for classes (instances of type, including metaclasses)."""
    return isinstance(object, type)

def ismethod(object):
    """True for bound methods of Python functions."""
    return isinstance(object, types.MethodType)

def ispackage(object):
    """True for modules that are packages (have a __path__)."""
    return ismodule(object) and hasattr(object, "__path__")

def ismethoddescriptor(object):
    """True for method descriptors such as str.join or int.__add__:
    objects with __get__ but without __set__/__delete__, that are not
    classes, methods or functions."""
    if isclass(object) or ismethod(object) or isfunction(object):
        return False
    kind = type(object)
    return (hasattr(kind, "__get__") and not hasattr(kind, "__set__")
            and not hasattr(kind, "__delete__"))

def isdatadescriptor(object):
    """True for data descriptors (types defining __set__ or __delete__)."""
    if isclass(object) or ismethod(object) or isfunction(object):
        return False
    kind = type(object)
    return hasattr(kind, "__set__") or hasattr(kind, "__delete__")

if hasattr(types, "MemberDescriptorType"):
    def ismemberdescriptor(object):
        """True for descriptors of __slots__ / C struct members."""
        return isinstance(object, types.MemberDescriptorType)
else:
    def ismemberdescriptor(object):
        """True for descriptors of __slots__ / C struct members."""
        return False

if hasattr(types, "GetSetDescriptorType"):
    def isgetsetdescriptor(object):
        """True for getset descriptors defined in extension modules."""
        return isinstance(object, types.GetSetDescriptorType)
else:
    def isgetsetdescriptor(object):
        """True for getset descriptors defined in extension modules."""
        return False

def isfunction(object):
    """True for Python functions (including lambdas)."""
    return isinstance(object, types.FunctionType)


def _unwrap_callable_for_flags(f):
    f = functools._unwrap_partialmethod(f)
    while ismethod(f):
        f = f.__func__
    return functools._unwrap_partial(f)

def _has_code_flag(f, flag):
    f = _unwrap_callable_for_flags(f)
    if not (isfunction(f) or _signature_is_functionlike(f)):
        return False
    return bool(f.__code__.co_flags & flag)

def isgeneratorfunction(obj):
    """True for functions defined with def that contain yield."""
    return _has_code_flag(obj, CO_GENERATOR)

# Attribute set by markcoroutinefunction().
_is_coroutine_mark = object()

def _has_coroutine_mark(f):
    while ismethod(f):
        f = f.__func__
    f = functools._unwrap_partial(f)
    return getattr(f, "_is_coroutine_marker", None) is _is_coroutine_mark

def markcoroutinefunction(func):
    """Decorator: make iscoroutinefunction() report *func* as a
    coroutine function (for sync callables returning awaitables)."""
    if hasattr(func, '__func__'):
        func = func.__func__
    func._is_coroutine_marker = _is_coroutine_mark
    return func

def iscoroutinefunction(obj):
    """True for 'async def' functions (or ones marked with
    markcoroutinefunction())."""
    return _has_code_flag(obj, CO_COROUTINE) or _has_coroutine_mark(obj)

def isasyncgenfunction(obj):
    """True for 'async def' functions containing yield."""
    return _has_code_flag(obj, CO_ASYNC_GENERATOR)

def isasyncgen(object):
    """True for asynchronous generator objects."""
    return isinstance(object, types.AsyncGeneratorType)

def isgenerator(object):
    """True for generator objects."""
    return isinstance(object, types.GeneratorType)

def iscoroutine(object):
    """True for coroutine objects created by 'async def' functions."""
    return isinstance(object, types.CoroutineType)

def isawaitable(object):
    """True if *object* can be used in an await expression."""
    return (isinstance(object, types.CoroutineType)
            or (isinstance(object, types.GeneratorType)
                and bool(object.gi_code.co_flags & CO_ITERABLE_COROUTINE))
            or isinstance(object, collections.abc.Awaitable))

def istraceback(object):
    """True for traceback objects."""
    return isinstance(object, types.TracebackType)

def isframe(object):
    """True for frame objects."""
    return isinstance(object, types.FrameType)

def iscode(object):
    """True for code objects."""
    return isinstance(object, types.CodeType)

def isbuiltin(object):
    """True for built-in functions and methods (bound C functions)."""
    return isinstance(object, types.BuiltinFunctionType)

def ismethodwrapper(object):
    """True for method-wrapper objects (e.g. object().__str__)."""
    return isinstance(object, types.MethodWrapperType)

def isroutine(object):
    """True for any kind of function or method."""
    return (isbuiltin(object)
            or isfunction(object)
            or ismethod(object)
            or ismethoddescriptor(object)
            or ismethodwrapper(object)
            or isinstance(object, functools.partial)
            or isinstance(object, _DISPATCH_GETTERS))

# Objects produced by functools.singledispatchmethod.__get__ act as methods.
_DISPATCH_GETTERS = tuple(
    getattr(functools, name) for name in ("_singledispatchmethod_get",)
    if isinstance(getattr(functools, name, None), type))

def isabstract(object):
    """True for abstract base classes that cannot be instantiated."""
    if not isinstance(object, type):
        return False
    if object.__flags__ & TPFLAGS_IS_ABSTRACT:
        return True
    if not issubclass(type(object), abc.ABCMeta):
        return False
    if hasattr(object, '__abstractmethods__'):
        # ABCMeta.__new__ already computed it: the flag above was right.
        return False
    # Called from __init_subclass__ before ABCMeta.__new__ finished: look
    # for abstract methods ourselves.
    for name, value in object.__dict__.items():
        if getattr(value, "__isabstractmethod__", False):
            return True
    for base in object.__bases__:
        for name in getattr(base, "__abstractmethods__", ()):
            value = getattr(object, name, None)
            if getattr(value, "__isabstractmethod__", False):
                return True
    return False


# --------------------------------------------------------------------------
# Members

def _getmembers(object, predicate, getter):
    results = []
    processed = set()
    names = dir(object)
    if isclass(object):
        mro = getmro(object)
        # Attributes found through __getattr__ of a metaclass (e.g. enum
        # members on DynamicClassAttribute) are listed via the bases too.
        try:
            for base in object.__bases__:
                for key, value in base.__dict__.items():
                    if isinstance(value, types.DynamicClassAttribute):
                        names.append(key)
        except AttributeError:
            pass
    else:
        mro = ()
    for key in names:
        try:
            value = getter(object, key)
            if key in processed:
                raise AttributeError
        except AttributeError:
            for base in mro:
                if key in base.__dict__:
                    value = base.__dict__[key]
                    break
            else:
                # dir() reported a name that cannot be retrieved.
                continue
        if not predicate or predicate(value):
            results.append((key, value))
        processed.add(key)
    results.sort(key=lambda pair: pair[0])
    return results

def getmembers(object, predicate=None):
    """Return all (name, value) members of *object*, sorted by name,
    optionally only those for which predicate(value) is true."""
    return _getmembers(object, predicate, getattr)

def getmembers_static(object, predicate=None):
    """Like getmembers() but without triggering descriptors or
    __getattr__ (uses getattr_static())."""
    return _getmembers(object, predicate, getattr_static)


Attribute = collections.namedtuple('Attribute', 'name kind defining_class object')

def classify_class_attrs(cls):
    """List Attribute(name, kind, defining_class, object) tuples for
    every attribute of *cls*.  kind is one of 'class method',
    'static method', 'property', 'method' or 'data'."""
    mro = getmro(cls)
    metamro = getmro(type(cls))
    metamro = tuple(c for c in metamro if c not in (type, object))
    class_bases = (cls,) + mro
    all_bases = class_bases + metamro
    names = dir(cls)
    for base in mro:
        for key, value in base.__dict__.items():
            if isinstance(value, types.DynamicClassAttribute):
                names.append(key)
    result = []
    seen = set()
    for name in names:
        homecls = None
        get_obj = None
        dict_obj = None
        if name not in seen:
            try:
                if name == '__dict__':
                    raise Exception("__dict__ is special, don't want the proxy")
                get_obj = getattr(cls, name)
            except Exception:
                pass
            else:
                homecls = getattr(get_obj, "__objclass__", homecls)
                if homecls not in class_bases:
                    # The attribute came from the metaclass, or __objclass__
                    # is unrelated: search the class hierarchy instead.
                    homecls = None
                    last_cls = None
                    for srch_cls in class_bases:
                        srch_obj = getattr(srch_cls, name, None)
                        if srch_obj is get_obj:
                            last_cls = srch_cls
                    for srch_cls in metamro:
                        try:
                            srch_obj = srch_cls.__getattr__(cls, name)
                        except AttributeError:
                            continue
                        if srch_obj is get_obj:
                            last_cls = srch_cls
                    if last_cls is not None:
                        homecls = last_cls
        for base in all_bases:
            if name in base.__dict__:
                dict_obj = base.__dict__[name]
                if homecls not in metamro:
                    homecls = base
                break
        if homecls is None:
            continue
        obj = get_obj if get_obj is not None else dict_obj
        if isinstance(dict_obj, (staticmethod, types.BuiltinMethodType)):
            kind = "static method"
            obj = dict_obj
        elif isinstance(dict_obj, (classmethod, types.ClassMethodDescriptorType)):
            kind = "class method"
            obj = dict_obj
        elif isinstance(dict_obj, property):
            kind = "property"
            obj = dict_obj
        elif isroutine(obj):
            kind = "method"
        else:
            kind = "data"
        result.append(Attribute(name, kind, homecls, obj))
        seen.add(name)
    return result


def getmro(cls):
    """Method resolution order of *cls* as a tuple."""
    return cls.__mro__


def unwrap(func, *, stop=None):
    """Follow the chain of __wrapped__ attributes (as set by
    functools.wraps) and return the innermost object.

    *stop* is called on each object; when it returns true the unwrapping
    stops there.  ValueError is raised for cycles.
    """
    current = func
    seen = {id(current)}
    limit = sys.getrecursionlimit()
    while not isinstance(current, type) and hasattr(current, '__wrapped__'):
        if stop is not None and stop(current):
            break
        current = current.__wrapped__
        ident = id(current)
        if ident in seen or len(seen) >= limit:
            raise ValueError('wrapper loop when unwrapping {!r}'.format(func))
        seen.add(ident)
    return current


# --------------------------------------------------------------------------
# Docstrings and comments

def indentsize(line):
    """Number of leading whitespace characters (tabs expanded)."""
    expanded = line.expandtabs()
    return len(expanded) - len(expanded.lstrip())


def _findclass(func):
    cls = sys.modules.get(func.__module__)
    if cls is None:
        return None
    for name in func.__qualname__.split('.')[:-1]:
        cls = getattr(cls, name)
    if not isclass(cls):
        return None
    return cls


def _finddoc(obj, *, search_in_class=True):
    """Look for an inherited docstring of *obj*."""
    if isclass(obj):
        if not search_in_class:
            return None
        for base in obj.__mro__:
            if base is object:
                continue
            try:
                doc = base.__doc__
            except AttributeError:
                continue
            if doc is not None:
                return doc
        return None

    if ismethod(obj):
        name = obj.__func__.__name__
        owner = obj.__self__
        if (isclass(owner) and
                getattr(getattr(owner, name, None), '__func__') is obj.__func__):
            cls = owner                  # classmethod
        else:
            cls = owner.__class__
    elif isfunction(obj):
        name = obj.__name__
        cls = _findclass(obj)
        if cls is None or getattr(cls, name) is not obj:
            return None
    elif isbuiltin(obj):
        name = obj.__name__
        owner = obj.__self__
        if (isclass(owner) and
                owner.__qualname__ + '.' + name == obj.__qualname__):
            cls = owner                  # classmethod
        else:
            cls = owner.__class__
    elif isinstance(obj, property):
        name = obj.__name__
        cls = _findclass(obj.fget)
        if cls is None or getattr(cls, name) is not obj:
            return None
    elif isinstance(obj, functools.cached_property):
        name = obj.attrname
        cls = _findclass(obj.func)
        if cls is None or getattr(cls, name) is not obj:
            return None
    elif ismethoddescriptor(obj) or isdatadescriptor(obj):
        name = obj.__name__
        cls = obj.__objclass__
        if getattr(cls, name) is not obj:
            return None
        if ismemberdescriptor(obj):
            slots = getattr(cls, '__slots__', None)
            if isinstance(slots, dict) and name in slots:
                return slots[name]
    else:
        return None

    kind = type(obj)
    for base in cls.__mro__:
        try:
            candidate = base.__dict__[name] if name in base.__dict__ else getattr(base, name)
        except AttributeError:
            continue
        if isinstance(obj, (property, functools.cached_property)) and \
                not isinstance(candidate, kind):
            # Redefined as something else further down: stop looking.
            if base is not cls:
                break
        doc = getattr(candidate, '__doc__', None)
        if doc is not None:
            return doc
    return None


def getdoc(object, *, inherit_class_doc=True, fallback_to_class_doc=True):
    """Return the cleaned-up documentation string of *object*.

    Missing docstrings of classes, methods and properties are looked up
    in base classes (inherit_class_doc).  For other objects the class
    docstring is used unless fallback_to_class_doc is false.
    """
    try:
        doc = object.__doc__
    except AttributeError:
        return None
    if (not fallback_to_class_doc and not isclass(object)
            and not ismodule(object) and not isroutine(object)):
        own = getattr(object, '__dict__', None)
        if not (isinstance(own, dict) and '__doc__' in own):
            if doc is getattr(type(object), '__doc__', None):
                doc = None
    if doc is None:
        if isclass(object) or isroutine(object) or isdatadescriptor(object) \
                or isinstance(object, functools.cached_property):
            try:
                doc = _finddoc(object, search_in_class=inherit_class_doc)
            except (AttributeError, TypeError):
                return None
    if not isinstance(doc, str):
        return None
    return cleandoc(doc)


def cleandoc(doc):
    """Remove the indentation docstrings get from their position in
    the source: the first line is stripped of leading whitespace, the
    common indentation of the remaining lines is removed, and blank
    lines at the start and end are dropped."""
    lines = doc.expandtabs().split('\n')
    margin = None
    for line in lines[1:]:
        stripped = line.lstrip(' ')
        if stripped:
            indent = len(line) - len(stripped)
            margin = indent if margin is None else min(margin, indent)
    if lines:
        lines[0] = lines[0].lstrip(' ')
    if margin is not None:
        lines[1:] = [line[margin:] for line in lines[1:]]
    while lines and not lines[-1]:
        lines.pop()
    while lines and not lines[0]:
        lines.pop(0)
    return '\n'.join(lines)


# --------------------------------------------------------------------------
# Files and modules

def getfile(object):
    """Name of the (source or compiled) file *object* was defined in."""
    if ismodule(object):
        if getattr(object, '__file__', None):
            return object.__file__
        raise TypeError('{!r} is a built-in module'.format(object))
    if isclass(object):
        if hasattr(object, '__module__'):
            module = sys.modules.get(object.__module__)
            if getattr(module, '__file__', None):
                return module.__file__
            if object.__module__ == '__main__':
                raise OSError('source code not available')
        raise TypeError('{!r} is a built-in class'.format(object))
    if ismethod(object):
        object = object.__func__
    if isfunction(object):
        object = object.__code__
    if istraceback(object):
        object = object.tb_frame
    if isframe(object):
        object = object.f_code
    if iscode(object):
        return object.co_filename
    raise TypeError('module, class, method, function, traceback, frame, or '
                    'code object was expected, got {}'.format(
                    type(object).__name__))


def getmodulename(path):
    """Module name for a file path, or None if it has no module suffix."""
    filename = os.path.basename(path)
    suffixes = [(-len(suffix), suffix)
                for suffix in importlib.machinery.all_suffixes()]
    suffixes.sort()
    for neglen, suffix in suffixes:
        if filename.endswith(suffix):
            return filename[:neglen]
    return None


def getsourcefile(object):
    """Path of the Python source file of *object*, or None."""
    filename = getfile(object)
    all_bytecode = importlib.machinery.BYTECODE_SUFFIXES[:]
    if any(filename.endswith(s) for s in all_bytecode):
        filename = (os.path.splitext(filename)[0] +
                    importlib.machinery.SOURCE_SUFFIXES[0])
    elif any(filename.endswith(s) for s in
             importlib.machinery.EXTENSION_SUFFIXES):
        return None
    elif filename.endswith(".fwork"):
        raise OSError("source code not available")
    if os.path.exists(filename):
        return filename
    module = getmodule(object, filename)
    if getattr(module, '__loader__', None) is not None:
        return filename
    if getattr(getattr(module, "__spec__", None), "loader", None) is not None:
        return filename
    if filename in linecache.cache:
        return filename
    return None


def getabsfile(object, _filename=None):
    """Absolute, normalised file name of *object*'s source (or
    compiled) file, used as a key to find the module."""
    if _filename is None:
        _filename = getsourcefile(object) or getfile(object)
    return os.path.normcase(os.path.abspath(_filename))


# Caches used by getmodule(): absolute file name -> module name, and
# module name -> last seen __file__ (to notice changes).
modulesbyfile = {}
_filesbymodname = {}


def getmodule(object, _filename=None):
    """Return the module *object* was defined in, or None."""
    if ismodule(object):
        return object
    if hasattr(object, '__module__'):
        return sys.modules.get(object.__module__)
    if _filename is not None and _filename in modulesbyfile:
        return sys.modules.get(modulesbyfile[_filename])
    try:
        file = getabsfile(object, _filename)
    except (TypeError, FileNotFoundError):
        return None
    if file in modulesbyfile:
        return sys.modules.get(modulesbyfile[file])
    # Refresh the cache with modules imported since the last call.
    for modname, module in sys.modules.copy().items():
        if ismodule(module) and hasattr(module, '__file__'):
            known = module.__file__
            if known == _filesbymodname.get(modname, None):
                continue
            _filesbymodname[modname] = known
            path = getabsfile(module)
            modulesbyfile[path] = modulesbyfile[
                os.path.realpath(path)] = module.__name__
    if file in modulesbyfile:
        return sys.modules.get(modulesbyfile[file])
    main = sys.modules['__main__']
    if not hasattr(object, '__name__'):
        return None
    if hasattr(main, object.__name__):
        if getattr(main, object.__name__) is object:
            return main
    builtin = sys.modules['builtins']
    if hasattr(builtin, object.__name__):
        if getattr(builtin, object.__name__) is object:
            return builtin
    return None


class ClassFoundException(Exception):
    pass


def _code_of(object):
    """The code object behind a function, method, frame, traceback or code
    object (None for anything else)."""
    if ismethod(object):
        object = object.__func__
    if isfunction(object):
        return object.__code__
    if istraceback(object):
        object = object.tb_frame
    if isframe(object):
        return object.f_code
    if iscode(object):
        return object
    return None


def findsource(object):
    """Return (all lines of the source file, index of the first line of
    *object*).  OSError is raised if the source is unavailable."""
    file = getsourcefile(object)
    if file:
        linecache.checkcache(file)
    else:
        file = getfile(object)
        # Synthetic file names such as "<string>" may still be served by
        # linecache (doctest, the REPL), so keep going.
        if not (file.startswith('<') and file.endswith('>')):
            raise OSError('source code not available')

    module = getmodule(object, file)
    if module:
        lines = linecache.getlines(file, module.__dict__)
    else:
        lines = linecache.getlines(file)
    if not lines:
        # Code typed at the interactive prompt is registered with linecache
        # per code object rather than per file name.
        code = _code_of(object)
        if code is not None:
            lines = linecache._getlines_from_code(code)
    if not lines:
        raise OSError('could not get source code')

    if ismodule(object):
        return lines, 0

    if isclass(object):
        try:
            lnum = vars(object)['__firstlineno__'] - 1
        except (TypeError, KeyError):
            raise OSError('source code not available')
        if lnum >= len(lines):
            raise OSError('lineno is out of bounds')
        return lines, lnum

    if ismethod(object):
        object = object.__func__
    if isfunction(object):
        object = object.__code__
    if istraceback(object):
        object = object.tb_frame
    if isframe(object):
        object = object.f_code
    if iscode(object):
        if not hasattr(object, 'co_firstlineno'):
            raise OSError('could not find function definition')
        lnum = object.co_firstlineno - 1
        if lnum >= len(lines):
            raise OSError('lineno is out of bounds')
        return lines, lnum
    raise OSError('could not find code object')


def getcomments(object):
    """Comment lines immediately preceding *object*'s source (or at the
    top of a module), as one string; None if there are none."""
    try:
        lines, lnum = findsource(object)
    except (OSError, TypeError):
        return None

    if ismodule(object):
        # Leading comments of the file, after any shebang line.
        start = 0
        if lines and lines[0][:2] == '#!':
            start = 1
        while start < len(lines) and lines[start].strip() in ('', '#'):
            start += 1
        if start < len(lines) and lines[start][:1] == '#':
            comments = []
            end = start
            while end < len(lines) and lines[end][:1] == '#':
                comments.append(lines[end].expandtabs())
                end += 1
            return ''.join(comments)
        return None

    if lnum > 0:
        indent = indentsize(lines[lnum])
        end = lnum - 1
        if end >= 0 and lines[end].lstrip()[:1] == '#' and \
                indentsize(lines[end]) == indent:
            comments = [lines[end].expandtabs().lstrip()]
            if end > 0:
                end -= 1
                comment = lines[end].expandtabs().lstrip()
                while comment[:1] == '#' and indentsize(lines[end]) == indent:
                    comments[:0] = [comment]
                    end -= 1
                    if end < 0:
                        break
                    comment = lines[end].expandtabs().lstrip()
            while comments and comments[0].strip() == '#':
                comments[:1] = []
            while comments and comments[-1].strip() == '#':
                comments[-1:] = []
            return ''.join(comments)
    return None


class EndOfBlock(Exception):
    pass


class BlockFinder:
    """Feed tokens (from tokenize) to find the end of a def/class/lambda
    block starting at the first token."""

    def __init__(self):
        self.indent = 0
        self.islambda = False
        self.started = False
        self.passline = False
        self.indecorator = False
        self.last = 1
        self.body_col0 = None

    def tokeneater(self, type, token, srowcol, erowcol, line):
        import tokenize
        if not self.started and not self.indecorator:
            if type in (tokenize.INDENT, tokenize.COMMENT, tokenize.NL):
                pass
            elif token == "async":
                pass
            elif token == "@":
                self.indecorator = True
            elif token in ("def", "class", "lambda"):
                if token == "lambda":
                    self.islambda = True
                self.started = True
            elif type != tokenize.ENDMARKER:
                # Any other statement (e.g. the assignment holding a
                # multi-line generator expression) ends with its line.
                self.islambda = True
                self.started = True
            self.passline = True
        elif type == tokenize.NEWLINE:
            self.passline = False
            self.last = srowcol[0]
            if self.islambda:
                raise EndOfBlock
            if self.indecorator:
                self.indecorator = False
        elif self.passline:
            pass
        elif type == tokenize.INDENT:
            if self.body_col0 is None and self.started:
                self.body_col0 = erowcol[1]
            self.indent += 1
            self.passline = True
        elif type == tokenize.DEDENT:
            self.indent -= 1
            if self.indent <= 0:
                raise EndOfBlock
        elif type == tokenize.COMMENT:
            if self.body_col0 is not None and srowcol[1] >= self.body_col0:
                # Comment still inside the indented body.
                self.last = srowcol[0]
        elif self.indent == 0 and type not in (tokenize.COMMENT, tokenize.NL):
            # A statement at the same level: the block was a one-liner.
            raise EndOfBlock


def getblock(lines):
    """The lines making up the block that starts at lines[0]."""
    import tokenize
    finder = BlockFinder()
    try:
        tokens = tokenize.generate_tokens(iter(lines).__next__)
        for tok in tokens:
            finder.tokeneater(*tok)
    except (EndOfBlock, IndentationError, SyntaxError):
        pass
    except tokenize.TokenError:
        pass
    return lines[:finder.last]


def getsourcelines(object):
    """Return (list of source lines, starting line number) of *object*.
    For a module the whole file is returned with line number 0."""
    object = unwrap(object)
    lines, lnum = findsource(object)
    if istraceback(object):
        object = object.tb_frame
    if ismodule(object) or (isframe(object) and object.f_code.co_name == "<module>"):
        return lines, 0
    return getblock(lines[lnum:]), lnum + 1


def getsource(object):
    """The source text of *object* as one string."""
    lines, lnum = getsourcelines(object)
    return ''.join(lines)


# --------------------------------------------------------------------------
# Class trees

def walktree(classes, children, parent):
    """Helper for getclasstree(): nested lists below *parent*."""
    results = []
    classes.sort(key=attrgetter('__module__', '__name__'))
    for cls in classes:
        results.append((cls, cls.__bases__))
        if cls in children:
            results.append(walktree(children[cls], children, cls))
    return results


def getclasstree(classes, unique=False):
    """Arrange *classes* into a nested list by inheritance.  Each entry
    is (class, bases), followed by a list of its subclasses' entries.
    With unique true a class with several listed bases appears once."""
    children = {}
    roots = []
    for cls in classes:
        if cls.__bases__:
            for parent in cls.__bases__:
                kids = children.setdefault(parent, [])
                if cls not in kids:
                    kids.append(cls)
                if unique and parent in classes:
                    break
        elif cls not in roots:
            roots.append(cls)
    for parent in children:
        if parent not in classes:
            roots.append(parent)
    return walktree(roots, children, None)


# --------------------------------------------------------------------------
# Arguments

ArgInfo = collections.namedtuple('ArgInfo', 'args varargs keywords locals')
FullArgSpec = collections.namedtuple(
    'FullArgSpec',
    'args, varargs, varkw, defaults, kwonlyargs, kwonlydefaults, annotations')


def _getfullargs(co):
    if not iscode(co):
        raise TypeError('{!r} is not a code object'.format(co))
    names = co.co_varnames
    nargs = co.co_argcount
    nkwargs = co.co_kwonlyargcount
    npos = co.co_posonlyargcount
    args = list(names[:nargs])
    kwonly = list(names[nargs:nargs + nkwargs])
    step = nargs + nkwargs
    varargs = None
    if co.co_flags & CO_VARARGS:
        varargs = names[step]
        step += 1
    varkw = None
    if co.co_flags & CO_VARKEYWORDS:
        varkw = names[step]
    return args, npos, kwonly, varargs, varkw


def getargs(co):
    """(args, varargs, varkw) of a code object."""
    args, _npos, kwonly, varargs, varkw = _getfullargs(co)
    return Arguments(args + kwonly, varargs, varkw)

Arguments = collections.namedtuple('Arguments', 'args, varargs, varkw')


def getfullargspec(func, *, annotation_format=Format.VALUE):
    """Legacy view of a callable's signature as a FullArgSpec named
    tuple; positional-only parameters are folded into args."""
    try:
        sig = _signature_from_callable(func, follow_wrapper_chains=False,
                                       skip_bound_arg=False,
                                       sigcls=Signature, eval_str=False,
                                       annotation_format=annotation_format)
    except Exception as exc:
        raise TypeError('unsupported callable') from exc

    args = []
    varargs = None
    varkw = None
    posonly = []
    kwonly = []
    annotations = {}
    defaults = ()
    kwdefaults = {}
    if sig.return_annotation is not sig.empty:
        annotations['return'] = sig.return_annotation
    for param in sig.parameters.values():
        kind = param.kind
        name = param.name
        if kind is _POSITIONAL_ONLY:
            posonly.append(name)
            if param.default is not param.empty:
                defaults += (param.default,)
        elif kind is _POSITIONAL_OR_KEYWORD:
            args.append(name)
            if param.default is not param.empty:
                defaults += (param.default,)
        elif kind is _VAR_POSITIONAL:
            varargs = name
        elif kind is _KEYWORD_ONLY:
            kwonly.append(name)
            if param.default is not param.empty:
                kwdefaults[name] = param.default
        elif kind is _VAR_KEYWORD:
            varkw = name
        if param.annotation is not param.empty:
            annotations[name] = param.annotation
    return FullArgSpec(posonly + args, varargs, varkw, defaults or None,
                       kwonly, kwdefaults or None, annotations)


def getargvalues(frame):
    """ArgInfo(args, varargs, varkw, locals) for the call in *frame*."""
    args, varargs, varkw = getargs(frame.f_code)
    return ArgInfo(args, varargs, varkw, frame.f_locals)


def formatannotation(annotation, base_module=None, *, quote_annotation_strings=True):
    """Render an annotation for display in a signature."""
    if not quote_annotation_strings and isinstance(annotation, str):
        return annotation
    if getattr(annotation, '__module__', None) == 'typing':
        def repl(match):
            text = match.group()
            return text.removeprefix('typing.')
        import re
        return re.sub(r'[\w\.]+', repl, repr(annotation))
    if isinstance(annotation, types.GenericAlias):
        return str(annotation)
    if isinstance(annotation, type):
        if annotation.__module__ in ('builtins', base_module):
            return annotation.__qualname__
        return annotation.__module__ + '.' + annotation.__qualname__
    if isinstance(annotation, ForwardRef):
        return annotation.__forward_arg__
    return repr(annotation)


def formatannotationrelativeto(object):
    """formatannotation() variant omitting *object*'s module prefix."""
    module = getattr(object, '__module__', None)

    def _formatannotation(annotation):
        return formatannotation(annotation, module)
    return _formatannotation


def formatargvalues(args, varargs, varkw, locals,
                    formatarg=str,
                    formatvarargs=lambda name: '*' + name,
                    formatvarkw=lambda name: '**' + name,
                    formatvalue=lambda value: '=' + repr(value)):
    """Format the result of getargvalues() as "(a=1, *b=(), **c={})"."""
    def convert(name):
        return formatarg(name) + formatvalue(locals[name])
    parts = [convert(name) for name in args]
    if varargs:
        parts.append(formatvarargs(varargs) + formatvalue(locals[varargs]))
    if varkw:
        parts.append(formatvarkw(varkw) + formatvalue(locals[varkw]))
    return '(' + ', '.join(parts) + ')'


def _missing_arguments(f_name, argnames, pos, values):
    names = [repr(name) for name in argnames if name not in values]
    missing = len(names)
    if missing == 1:
        listing = names[0]
    elif missing == 2:
        listing = "{} and {}".format(*names)
    else:
        tail = ", {} and {}".format(*names[-2:])
        del names[-2:]
        listing = ", ".join(names) + tail
    raise TypeError("%s() missing %i required %s argument%s: %s" %
                    (f_name, missing,
                     "positional" if pos else "keyword-only",
                     "" if missing == 1 else "s", listing))


def _too_many(f_name, args, kwonly, varargs, defcount, given, values):
    atleast = len(args) - defcount
    kwonly_given = len([arg for arg in kwonly if arg in values])
    if varargs:
        plural = atleast != 1
        sig = "at least %d" % (atleast,)
    elif defcount:
        plural = True
        sig = "from %d to %d" % (atleast, len(args))
    else:
        plural = len(args) != 1
        sig = str(len(args))
    kwonly_sig = ""
    if kwonly_given:
        msg = " positional argument%s (and %d keyword-only argument%s)"
        kwonly_sig = (msg % ("s" if given != 1 else "", kwonly_given,
                             "s" if kwonly_given != 1 else ""))
    raise TypeError("%s() takes %s positional argument%s but %d%s %s given" %
                    (f_name, sig, "s" if plural else "", given, kwonly_sig,
                     "was" if given == 1 and not kwonly_given else "were"))


def getcallargs(func, /, *positional, **named):
    """Map the arguments of a call func(*positional, **named) to the
    parameter names of *func*, like the interpreter would (deprecated
    in favour of Signature.bind())."""
    spec = getfullargspec(func)
    args, varargs, varkw, defaults, kwonlyargs, kwonlydefaults, ann = spec
    f_name = func.__name__
    arg2value = {}

    if ismethod(func) and func.__self__ is not None:
        positional = (func.__self__,) + positional
    num_pos = len(positional)
    num_args = len(args)
    num_defaults = len(defaults) if defaults else 0

    n = min(num_pos, num_args)
    for i in range(n):
        arg2value[args[i]] = positional[i]
    if varargs:
        arg2value[varargs] = tuple(positional[n:])
    possible_kwargs = set(args + kwonlyargs)
    if varkw:
        arg2value[varkw] = {}
    for kw, value in named.items():
        if kw not in possible_kwargs:
            if not varkw:
                raise TypeError("%s() got an unexpected keyword argument %r" %
                                (f_name, kw))
            arg2value[varkw][kw] = value
            continue
        if kw in arg2value:
            raise TypeError("%s() got multiple values for argument %r" %
                            (f_name, kw))
        arg2value[kw] = value
    if num_pos > num_args and not varargs:
        _too_many(f_name, args, kwonlyargs, varargs, num_defaults,
                  num_pos, arg2value)
    if num_pos < num_args:
        req = args[:num_args - num_defaults]
        for arg in req:
            if arg not in arg2value:
                _missing_arguments(f_name, req, True, arg2value)
        for i, arg in enumerate(args[num_args - num_defaults:]):
            if arg not in arg2value:
                arg2value[arg] = defaults[i]
    missing = 0
    for kwarg in kwonlyargs:
        if kwarg not in arg2value:
            if kwonlydefaults and kwarg in kwonlydefaults:
                arg2value[kwarg] = kwonlydefaults[kwarg]
            else:
                missing += 1
    if missing:
        _missing_arguments(f_name, kwonlyargs, False, arg2value)
    return arg2value


ClosureVars = collections.namedtuple('ClosureVars', 'nonlocals globals builtins unbound')

def getclosurevars(func):
    """ClosureVars(nonlocals, globals, builtins, unbound): how the
    free and global names of a Python function currently resolve."""
    if ismethod(func):
        func = func.__func__
    if not isfunction(func):
        raise TypeError("{!r} is not a Python function".format(func))
    code = func.__code__
    if func.__closure__ is None:
        nonlocal_vars = {}
    else:
        nonlocal_vars = {
            var: cell.cell_contents
            for var, cell in zip(code.co_freevars, func.__closure__)
        }
    global_ns = func.__globals__
    builtin_ns = global_ns.get("__builtins__", builtins.__dict__)
    if ismodule(builtin_ns):
        builtin_ns = builtin_ns.__dict__
    global_vars = {}
    builtin_vars = {}
    unbound_names = set()
    global_names = set()
    import dis
    for instruction in dis.get_instructions(code):
        opname = instruction.opname
        name = instruction.argval
        if opname == "LOAD_ATTR" or opname == "LOAD_METHOD":
            unbound_names.add(name)
        elif opname == "LOAD_GLOBAL":
            global_names.add(name)
    for const in code.co_consts:
        if iscode(const):
            for instruction in dis.get_instructions(const):
                if instruction.opname == "LOAD_GLOBAL":
                    global_names.add(instruction.argval)
    for name in global_names:
        try:
            global_vars[name] = global_ns[name]
        except KeyError:
            try:
                builtin_vars[name] = builtin_ns[name]
            except KeyError:
                unbound_names.add(name)
    return ClosureVars(nonlocal_vars, global_vars, builtin_vars, unbound_names)


# --------------------------------------------------------------------------
# Stack frames

_Traceback = collections.namedtuple(
    '_Traceback', 'filename lineno function code_context index')

class Traceback(_Traceback):
    """getframeinfo() result; also carries source positions."""

    def __new__(cls, filename, lineno, function, code_context, index, *,
                positions=None):
        instance = super().__new__(cls, filename, lineno, function,
                                   code_context, index)
        instance.positions = positions
        return instance

    def __repr__(self):
        return ('Traceback(filename={!r}, lineno={!r}, function={!r}, '
                'code_context={!r}, index={!r}, positions={!r})'.format(
                    self.filename, self.lineno, self.function,
                    self.code_context, self.index, self.positions))


def _positions_of(code, instruction_index):
    if instruction_index < 0:
        return (None,) * 4
    try:
        positions = code.co_positions()
        return next(itertools.islice(positions, instruction_index // 2, None))
    except (StopIteration, AttributeError, TypeError):
        return (None,) * 4


def getframeinfo(frame, context=1):
    """Traceback(filename, lineno, function, code_context, index) for a
    frame or traceback; code_context holds *context* lines around the
    current line."""
    import dis
    if istraceback(frame):
        positions = _positions_of(frame.tb_frame.f_code, frame.tb_lasti)
        lineno = frame.tb_lineno
        frame = frame.tb_frame
    else:
        lineno = frame.f_lineno
        positions = _positions_of(frame.f_code, frame.f_lasti)
    if positions[0] is None:
        positions = (lineno,) + tuple(positions[1:])
    if not isframe(frame):
        raise TypeError('{!r} is not a frame or traceback object'.format(frame))

    filename = getsourcefile(frame) or getfile(frame)
    if context > 0:
        start = lineno - 1 - context // 2
        try:
            lines, lnum = findsource(frame)
        except OSError:
            lines = index = None
        else:
            start = max(0, min(start, len(lines) - context))
            lines = lines[start:start + context]
            index = lineno - 1 - start
    else:
        lines = index = None
    return Traceback(filename, lineno, frame.f_code.co_name, lines, index,
                     positions=dis.Positions(*positions))


def getlineno(frame):
    """Current line number of a frame."""
    return frame.f_lineno


_FrameInfo = collections.namedtuple(
    '_FrameInfo', ('frame',) + Traceback._fields)

class FrameInfo(_FrameInfo):
    """A frame together with its getframeinfo() data."""

    def __new__(cls, frame, filename, lineno, function, code_context, index,
                *, positions=None):
        instance = super().__new__(cls, frame, filename, lineno, function,
                                   code_context, index)
        instance.positions = positions
        return instance

    def __repr__(self):
        return ('FrameInfo(frame={!r}, filename={!r}, lineno={!r}, '
                'function={!r}, code_context={!r}, index={!r}, '
                'positions={!r})'.format(
                    self.frame, self.filename, self.lineno, self.function,
                    self.code_context, self.index, self.positions))


def getouterframes(frame, context=1):
    """FrameInfo for *frame* and every frame calling it, innermost first."""
    result = []
    while frame:
        info = getframeinfo(frame, context)
        result.append(FrameInfo(frame, *info, positions=info.positions))
        frame = frame.f_back
    return result


def getinnerframes(tb, context=1):
    """FrameInfo for the frames of traceback *tb*, outermost first."""
    result = []
    while tb:
        info = getframeinfo(tb, context)
        result.append(FrameInfo(tb.tb_frame, *info, positions=info.positions))
        tb = tb.tb_next
    return result


def currentframe():
    """Frame of the caller (None if frames are not supported)."""
    return sys._getframe(1) if hasattr(sys, "_getframe") else None


def stack(context=1):
    """FrameInfo list for the caller's stack, innermost first."""
    return getouterframes(sys._getframe(1), context)


def trace(context=1):
    """FrameInfo list between the current frame and where the exception
    being handled was raised."""
    exc = sys.exception()
    tb = exc.__traceback__ if exc is not None else None
    return getinnerframes(tb, context)


# --------------------------------------------------------------------------
# Static attribute lookup

_sentinel = object()
_static_getmro = type.__dict__['__mro__'].__get__
_get_dunder_dict_of_class = type.__dict__["__dict__"].__get__


def _check_instance(obj, attr):
    try:
        instance_dict = object.__getattribute__(obj, "__dict__")
    except AttributeError:
        return _sentinel
    return dict.get(instance_dict, attr, _sentinel)


def _check_class(klass, attr):
    for entry in _static_getmro(klass):
        if _shadowed_dict(type(entry)) is _sentinel and attr in entry.__dict__:
            return entry.__dict__[attr]
    return _sentinel


@functools.lru_cache()
def _shadowed_dict_from_weakref_mro_tuple(*weakref_mro):
    for weakref_entry in weakref_mro:
        entry = weakref_entry()
        if entry is None:
            continue
        dunder_dict = _get_dunder_dict_of_class(entry)
        if '__dict__' in dunder_dict:
            class_dict = dunder_dict['__dict__']
            if not (type(class_dict) is types.GetSetDescriptorType and
                    class_dict.__name__ == "__dict__" and
                    (class_dict.__objclass__ in (entry, object)
                     or entry is type)):
                return class_dict
    return _sentinel


def _shadowed_dict(klass):
    # Weak references keep the cache from keeping classes alive.
    return _shadowed_dict_from_weakref_mro_tuple(
        *[weakref.ref(entry) for entry in _static_getmro(klass)])


def getattr_static(obj, attr, default=_sentinel):
    """Retrieve an attribute like getattr() would, but without running
    descriptors, __getattr__ or __getattribute__.

    Descriptor objects are returned as they are found.
    """
    instance_result = _sentinel

    objtype = type(obj)
    if type not in _static_getmro(objtype):
        klass = objtype
        dict_attr = _shadowed_dict(klass)
        if (dict_attr is _sentinel or
                type(dict_attr) is types.MemberDescriptorType):
            instance_result = _check_instance(obj, attr)
    else:
        klass = obj

    klass_result = _check_class(klass, attr)

    if instance_result is not _sentinel and klass_result is not _sentinel:
        if _check_class(type(klass_result), "__get__") is not _sentinel and (
            _check_class(type(klass_result), "__set__") is not _sentinel
            or _check_class(type(klass_result), "__delete__") is not _sentinel
        ):
            return klass_result

    if instance_result is not _sentinel:
        return instance_result
    if klass_result is not _sentinel:
        return klass_result

    if obj is klass:
        # Looking up a class attribute: also try the metaclass.
        for entry in _static_getmro(type(klass)):
            if _shadowed_dict(type(entry)) is _sentinel:
                try:
                    return entry.__dict__[attr]
                except KeyError:
                    pass
    if default is not _sentinel:
        return default
    raise AttributeError(attr)


# --------------------------------------------------------------------------
# Generator, coroutine and async generator state

GEN_CREATED = 'GEN_CREATED'
GEN_RUNNING = 'GEN_RUNNING'
GEN_SUSPENDED = 'GEN_SUSPENDED'
GEN_CLOSED = 'GEN_CLOSED'

def getgeneratorstate(generator):
    """One of GEN_CREATED, GEN_RUNNING, GEN_SUSPENDED, GEN_CLOSED."""
    if generator.gi_running:
        return GEN_RUNNING
    if generator.gi_suspended:
        return GEN_SUSPENDED
    if generator.gi_frame is None:
        return GEN_CLOSED
    return GEN_CREATED


def getgeneratorlocals(generator):
    """Local variables of a suspended generator (empty dict if closed)."""
    if not isgenerator(generator):
        raise TypeError("{!r} is not a Python generator".format(generator))
    frame = getattr(generator, "gi_frame", None)
    if frame is not None:
        return generator.gi_frame.f_locals
    return {}


CORO_CREATED = 'CORO_CREATED'
CORO_RUNNING = 'CORO_RUNNING'
CORO_SUSPENDED = 'CORO_SUSPENDED'
CORO_CLOSED = 'CORO_CLOSED'

def getcoroutinestate(coroutine):
    """One of CORO_CREATED, CORO_RUNNING, CORO_SUSPENDED, CORO_CLOSED."""
    if coroutine.cr_running:
        return CORO_RUNNING
    if coroutine.cr_suspended:
        return CORO_SUSPENDED
    if coroutine.cr_frame is None:
        return CORO_CLOSED
    return CORO_CREATED


def getcoroutinelocals(coroutine):
    """Local variables of a suspended coroutine (empty dict if closed)."""
    frame = getattr(coroutine, "cr_frame", None)
    if frame is not None:
        return frame.f_locals
    return {}


AGEN_CREATED = 'AGEN_CREATED'
AGEN_RUNNING = 'AGEN_RUNNING'
AGEN_SUSPENDED = 'AGEN_SUSPENDED'
AGEN_CLOSED = 'AGEN_CLOSED'

def getasyncgenstate(agen):
    """One of AGEN_CREATED, AGEN_RUNNING, AGEN_SUSPENDED, AGEN_CLOSED."""
    if agen.ag_running:
        return AGEN_RUNNING
    if agen.ag_suspended:
        return AGEN_SUSPENDED
    if agen.ag_frame is None:
        return AGEN_CLOSED
    return AGEN_CREATED


def getasyncgenlocals(agen):
    """Local variables of a suspended async generator."""
    if not isasyncgen(agen):
        raise TypeError(f"{agen!r} is not a Python async generator")
    frame = getattr(agen, "ag_frame", None)
    if frame is not None:
        return agen.ag_frame.f_locals
    return {}


# --------------------------------------------------------------------------
# Signatures

_WrapperDescriptor = type(type.__call__)
_MethodWrapper = type(all.__call__)
_ClassMethodWrapper = type(int.__dict__['from_bytes'])

_NonUserDefinedCallables = (_WrapperDescriptor,
                            _MethodWrapper,
                            _ClassMethodWrapper,
                            types.BuiltinFunctionType)


def _descriptor_get(descriptor, obj):
    if isclass(descriptor):
        return descriptor
    getter = getattr(type(descriptor), '__get__', _sentinel)
    if getter is _sentinel:
        return descriptor
    return getter(descriptor, obj, type(obj))


def _signature_get_user_defined_method(cls, method_name, *,
                                       follow_wrapper_chains=True):
    """The Python-level (or at least non-slot-wrapper) implementation of
    *method_name* on *cls*, bound appropriately; None if there is none."""
    if method_name == '__new__':
        method = getattr(cls, method_name, None)
    else:
        method = getattr_static(cls, method_name, None)
    if method is None:
        return None
    if follow_wrapper_chains and method_name != "__new__":
        # (__new__ is looked up bound; unwrapping a bound method would
        # lose the binding, so its wrapper chain is followed by the caller)
        method = unwrap(method, stop=(lambda m: hasattr(m, "__signature__")
                                      or _signature_is_builtin(m)))
    if isinstance(method, _NonUserDefinedCallables):
        return None
    if method_name != '__new__':
        method = _descriptor_get(method, cls)
    return method


def _signature_get_partial(wrapped_sig, partial, extra_args=()):
    """Signature of functools.partial(*partial*) given the signature of
    the wrapped callable."""
    old_params = wrapped_sig.parameters
    new_params = collections.OrderedDict(old_params.items())

    placeholder = getattr(functools, 'Placeholder', _sentinel)
    hole = object()
    partial_args = tuple(hole if arg is placeholder else arg
                         for arg in (partial.args or ()))
    partial_keywords = partial.keywords or {}
    if extra_args:
        partial_args = extra_args + partial_args

    try:
        bound = wrapped_sig.bind_partial(*partial_args, **partial_keywords)
    except TypeError as exc:
        raise ValueError('partial object {!r} has incorrect arguments'
                         .format(partial)) from exc

    became_kwonly = False
    for name, param in old_params.items():
        if name in bound.arguments:
            value = bound.arguments[name]
            if value is hole:
                # Left open by a Placeholder: still filled positionally.
                new_params[name] = param.replace(kind=_POSITIONAL_ONLY,
                                                 default=_empty)
                continue
            if param.kind is _POSITIONAL_ONLY:
                del new_params[name]
                continue
            if param.kind is _POSITIONAL_OR_KEYWORD:
                if name in partial_keywords:
                    # A keyword supplied by partial turns this and all
                    # following parameters into keyword-only ones.
                    became_kwonly = True
                    new_params[name] = param.replace(default=value)
                else:
                    del new_params[name]
                    continue
            if param.kind is _KEYWORD_ONLY:
                new_params[name] = param.replace(default=value)

        if became_kwonly:
            if param.kind is _POSITIONAL_OR_KEYWORD:
                new_params[name] = new_params[name].replace(kind=_KEYWORD_ONLY)
                new_params.move_to_end(name)
            elif param.kind in (_KEYWORD_ONLY, _VAR_KEYWORD):
                new_params.move_to_end(name)
            elif param.kind is _VAR_POSITIONAL:
                del new_params[name]

    return wrapped_sig.replace(parameters=new_params.values())


def _signature_bound_method(sig):
    """Drop the first (self) parameter of a signature."""
    params = tuple(sig.parameters.values())
    if not params or params[0].kind in (_VAR_KEYWORD, _KEYWORD_ONLY):
        raise ValueError('invalid method signature')
    if params[0].kind in (_POSITIONAL_OR_KEYWORD, _POSITIONAL_ONLY):
        params = params[1:]
    # A leading *args absorbs self; keep it.
    return sig.replace(parameters=params)


def _signature_is_builtin(obj):
    """True for callables implemented in C (incl. slot wrappers)."""
    # (functools.partial is a method descriptor but has its own signature rule)
    return (isbuiltin(obj) or
            (ismethoddescriptor(obj) and not isinstance(obj, functools.partial)) or
            isinstance(obj, _NonUserDefinedCallables) or
            obj is type or obj is object)


def _signature_is_functionlike(obj):
    """True for objects that quack like Python functions (Cython and
    similar compiled functions)."""
    if not callable(obj) or isclass(obj):
        return False
    name = getattr(obj, '__name__', None)
    code = getattr(obj, '__code__', None)
    defaults = getattr(obj, '__defaults__', _void)
    kwdefaults = getattr(obj, '__kwdefaults__', _void)
    return (isinstance(code, types.CodeType) and
            isinstance(name, str) and
            (defaults is None or isinstance(defaults, tuple)) and
            (kwdefaults is None or isinstance(kwdefaults, dict)))


def _signature_strip_non_python_syntax(signature):
    """Normalise a __text_signature__: drop the '$' marking the self
    parameter and collapse whitespace.  Returns (clean, self_index)."""
    if not signature:
        return signature, None

    tokens = []
    self_parameter = None
    position = 0
    depth = 0
    param_index = 0
    text = signature
    length = len(text)
    i = 0
    while i < length:
        ch = text[i]
        if ch.isspace():
            i += 1
            continue
        if ch in '\'"':
            quote = ch
            j = i + 1
            while j < length and text[j] != quote:
                j += 2 if text[j] == '\\' else 1
            tokens.append(text[i:j + 1])
            i = j + 1
            continue
        if ch.isalnum() or ch == '_' or ch == '.' and i + 1 < length and text[i + 1].isdigit():
            j = i
            while j < length and (text[j].isalnum() or text[j] in '_.'):
                j += 1
            if (j < length and text[j] in '\'"'
                    and text[i:j].lower() in ('b', 'r', 'u', 'rb', 'br')):
                # A prefixed string literal such as b'' stays one token.
                quote = text[j]
                j += 1
                while j < length and text[j] != quote:
                    j += 2 if text[j] == '\\' else 1
                j += 1
            tokens.append(text[i:j])
            i = j
            continue
        if ch == '$':
            if depth == 1 and self_parameter is None:
                self_parameter = param_index
            i += 1
            continue
        if ch in '([{':
            depth += 1
        elif ch in ')]}':
            depth -= 1
        elif ch == ',' and depth == 1:
            param_index += 1
        for op in ('**', '->', '...', '//', '<<', '>>'):
            if text.startswith(op, i):
                tokens.append(op)
                i += len(op)
                break
        else:
            tokens.append(ch)
            i += 1

    out = []
    previous = ''
    for tok in tokens:
        word = tok[0].isalnum() or tok[0] in '_\'"'
        if previous and word and (previous[-1].isalnum() or previous[-1] in '_\'"'):
            out.append(' ')
        out.append(tok)
        if tok == ',':
            out.append(' ')
        previous = tok
    clean = ''.join(out).replace(', )', ')').replace(', ]', ']')
    return clean, self_parameter


class _DefaultEvaluator:
    """Evaluates the restricted expressions allowed as defaults in
    __text_signature__ strings."""

    def __init__(self, obj):
        module_name = getattr(obj, '__module__', None)
        if not module_name:
            module_name = getattr(getattr(obj, '__objclass__', None),
                                  '__module__', None)
        module = sys.modules.get(module_name) if module_name else None
        self.namespace = module.__dict__ if module is not None else {}

    # Named defaults must resolve to plain constants.
    _CONSTANT_TYPES = (str, bytes, int, float, complex, bool, type(None))

    def lookup(self, dotted):
        head, *rest = dotted.split('.')
        if head in self.namespace:
            value = self.namespace[head]
        elif head in sys.modules:
            value = sys.modules[head]
        elif hasattr(builtins, head):
            value = getattr(builtins, head)
        else:
            raise ValueError(dotted)
        for part in rest:
            try:
                value = getattr(value, part)
            except AttributeError:
                raise ValueError(dotted) from None
        if not isinstance(value, self._CONSTANT_TYPES):
            raise ValueError(dotted)
        return value

    def evaluate(self, node):
        import ast
        if isinstance(node, ast.Constant):
            return node.value
        if isinstance(node, ast.Name):
            return self.lookup(node.id)
        if isinstance(node, ast.Attribute):
            parts = []
            while isinstance(node, ast.Attribute):
                parts.append(node.attr)
                node = node.value
            if not isinstance(node, ast.Name):
                raise ValueError("unsupported default")
            parts.append(node.id)
            return self.lookup('.'.join(reversed(parts)))
        if isinstance(node, ast.UnaryOp):
            operand = self.evaluate(node.operand)
            if isinstance(node.op, ast.USub):
                return -operand
            if isinstance(node.op, ast.UAdd):
                return +operand
            if isinstance(node.op, ast.Invert):
                return ~operand
            if isinstance(node.op, ast.Not):
                return not operand
        if isinstance(node, ast.BinOp):
            left = self.evaluate(node.left)
            right = self.evaluate(node.right)
            ops = {ast.Add: lambda a, b: a + b, ast.Sub: lambda a, b: a - b,
                   ast.Mult: lambda a, b: a * b, ast.BitOr: lambda a, b: a | b,
                   ast.BitAnd: lambda a, b: a & b,
                   ast.LShift: lambda a, b: a << b}
            for op_type, func in ops.items():
                if isinstance(node.op, op_type):
                    return func(left, right)
        if isinstance(node, (ast.Tuple, ast.List, ast.Set)):
            items = [self.evaluate(elt) for elt in node.elts]
            if isinstance(node, ast.Tuple):
                return tuple(items)
            return items if isinstance(node, ast.List) else set(items)
        if isinstance(node, ast.Dict):
            return {self.evaluate(k): self.evaluate(v)
                    for k, v in zip(node.keys, node.values)}
        raise ValueError("unsupported default")


def _signature_fromstr(cls, obj, s, skip_bound_arg=True):
    """Build a Signature from a __text_signature__ string."""
    import ast

    Parameter = cls._parameter_cls
    clean_signature, self_parameter = _signature_strip_non_python_syntax(s)
    program = "def foo" + clean_signature + ": pass"
    try:
        module = ast.parse(program)
    except SyntaxError:
        module = None
    if not isinstance(module, ast.Module):
        raise ValueError("{!r} builtin has invalid signature".format(obj))
    func = module.body[0]
    evaluator = _DefaultEvaluator(obj)
    parameters = []

    def add(arg, default_node, kind):
        if arg.annotation is not None:
            raise ValueError("Annotations are not currently supported")
        default = _empty
        if default_node is not None:
            try:
                default = evaluator.evaluate(default_node)
            except (ValueError, TypeError, ArithmeticError):
                raise ValueError("{!r} builtin has invalid signature"
                                 .format(obj)) from None
        parameters.append(Parameter(arg.arg, kind, default=default))

    args = func.args
    positional = list(args.posonlyargs) + list(args.args)
    defaults = [None] * (len(positional) - len(args.defaults)) + list(args.defaults)
    for index, (arg, default) in enumerate(zip(positional, defaults)):
        kind = _POSITIONAL_ONLY if index < len(args.posonlyargs) else _POSITIONAL_OR_KEYWORD
        add(arg, default, kind)
    if args.vararg:
        add(args.vararg, None, _VAR_POSITIONAL)
    for arg, default in zip(args.kwonlyargs, args.kw_defaults):
        add(arg, default, _KEYWORD_ONLY)
    if args.kwarg:
        add(args.kwarg, None, _VAR_KEYWORD)

    if self_parameter is not None:
        bound_to = getattr(obj, '__self__', None)
        if bound_to is not None and (ismodule(bound_to) or skip_bound_arg):
            parameters.pop(0)
        else:
            parameters[0] = parameters[0].replace(kind=_POSITIONAL_ONLY)

    return cls(parameters, return_annotation=cls.empty)


def _signature_from_builtin(cls, func, skip_bound_arg=True):
    """Signature of a C-implemented callable from its text signature."""
    if not _signature_is_builtin(func):
        raise TypeError("{!r} is not a Python builtin "
                        "function".format(func))
    text = getattr(func, "__text_signature__", None)
    if not text:
        raise ValueError("no signature found for builtin {!r}".format(func))
    return _signature_fromstr(cls, func, text, skip_bound_arg)


def _signature_from_function(cls, func, skip_bound_arg=True,
                             globals=None, locals=None, eval_str=False,
                             *, annotation_format=Format.VALUE):
    """Signature of a Python function (or function-like object)."""
    duck = False
    if not isfunction(func):
        if _signature_is_functionlike(func):
            duck = True
        else:
            raise TypeError('{!r} is not a Python function'.format(func))

    text = getattr(func, "__text_signature__", None)
    if text:
        return _signature_fromstr(cls, func, text, skip_bound_arg)

    Parameter = cls._parameter_cls
    code = func.__code__
    npos = code.co_argcount
    names = code.co_varnames
    nposonly = code.co_posonlyargcount
    nkwonly = code.co_kwonlyargcount
    positional = names[:npos]
    keyword_only = names[npos:npos + nkwonly]
    try:
        annotations = get_annotations(func, globals=globals, locals=locals,
                                      eval_str=eval_str,
                                      format=annotation_format)
    except TypeError:
        if not duck:
            raise
        annotations = getattr(func, '__annotations__', None) or {}
    defaults = func.__defaults__ or ()
    kwdefaults = func.__kwdefaults__ or {}

    parameters = []
    first_default = npos - len(defaults)
    for index, name in enumerate(positional):
        kind = _POSITIONAL_ONLY if index < nposonly else _POSITIONAL_OR_KEYWORD
        default = defaults[index - first_default] if index >= first_default else _empty
        parameters.append(Parameter(name, kind, default=default,
                                    annotation=annotations.get(name, _empty)))
    slot = npos + nkwonly
    if code.co_flags & CO_VARARGS:
        name = names[slot]
        slot += 1
        parameters.append(Parameter(name, _VAR_POSITIONAL,
                                    annotation=annotations.get(name, _empty)))
    for name in keyword_only:
        parameters.append(Parameter(name, _KEYWORD_ONLY,
                                    default=kwdefaults.get(name, _empty),
                                    annotation=annotations.get(name, _empty)))
    if code.co_flags & CO_VARKEYWORDS:
        name = names[slot]
        parameters.append(Parameter(name, _VAR_KEYWORD,
                                    annotation=annotations.get(name, _empty)))

    return cls(parameters,
               return_annotation=annotations.get('return', _empty),
               __validate_parameters__=duck)


def _signature_from_callable(obj, *,
                             follow_wrapper_chains=True,
                             skip_bound_arg=True,
                             globals=None,
                             locals=None,
                             eval_str=False,
                             sigcls,
                             annotation_format=Format.VALUE):
    """Compute the Signature of any callable."""

    def recurse(target):
        return _signature_from_callable(
            target, follow_wrapper_chains=follow_wrapper_chains,
            skip_bound_arg=skip_bound_arg, globals=globals, locals=locals,
            sigcls=sigcls, eval_str=eval_str,
            annotation_format=annotation_format)

    if not callable(obj):
        raise TypeError('{!r} is not a callable object'.format(obj))

    if isinstance(obj, types.MethodType):
        sig = recurse(obj.__func__)
        return _signature_bound_method(sig) if skip_bound_arg else sig

    if follow_wrapper_chains:
        obj = unwrap(obj, stop=(lambda f: hasattr(f, "__signature__")
                                or isinstance(f, types.MethodType)))
        if isinstance(obj, types.MethodType):
            return recurse(obj)

    try:
        sig = obj.__signature__
    except AttributeError:
        pass
    else:
        if sig is not None:
            original = sig
            if not isinstance(sig, (Signature, str)) and callable(sig):
                sig = sig()
            if isinstance(sig, str):
                sig = _signature_fromstr(sigcls, obj, sig)
            if not isinstance(sig, Signature):
                raise TypeError('unexpected object {!r} in __signature__ '
                                'attribute'.format(original))
            return sig

    try:
        partialmethod = obj.__partialmethod__
    except AttributeError:
        pass
    else:
        if isinstance(partialmethod, functools.partialmethod):
            wrapped_sig = recurse(partialmethod.func)
            sig = _signature_get_partial(wrapped_sig, partialmethod, (None,))
            first = tuple(wrapped_sig.parameters.values())[0]
            if first.kind is _VAR_POSITIONAL:
                return sig
            rest = tuple(sig.parameters.values())
            if rest and rest[0].kind is _POSITIONAL_ONLY:
                # Placeholders made the following parameters positional-only.
                first = first.replace(kind=_POSITIONAL_ONLY)
            return sig.replace(parameters=(first,) + rest)

    if isfunction(obj) or _signature_is_functionlike(obj):
        return _signature_from_function(sigcls, obj,
                                        skip_bound_arg=skip_bound_arg,
                                        globals=globals, locals=locals,
                                        eval_str=eval_str,
                                        annotation_format=annotation_format)

    if _signature_is_builtin(obj):
        return _signature_from_builtin(sigcls, obj,
                                       skip_bound_arg=skip_bound_arg)

    if isinstance(obj, functools.partial):
        return _signature_get_partial(recurse(obj.func), obj)

    if isinstance(obj, type):
        # A metaclass __call__ overrides everything.
        call = _signature_get_user_defined_method(
            type(obj), '__call__', follow_wrapper_chains=follow_wrapper_chains)
        if call is not None:
            return recurse(call)

        new = _signature_get_user_defined_method(
            obj, '__new__', follow_wrapper_chains=follow_wrapper_chains)
        init = _signature_get_user_defined_method(
            obj, '__init__', follow_wrapper_chains=follow_wrapper_chains)

        # Whichever of __new__/__init__ is defined closest in the MRO wins.
        for base in obj.__mro__:
            if new is not None and '__new__' in base.__dict__:
                sig = recurse(new)
                return _signature_bound_method(sig) if skip_bound_arg else sig
            if init is not None and '__init__' in base.__dict__:
                return recurse(init)

        for base in obj.__mro__[:-1]:
            try:
                text = base.__text_signature__
            except AttributeError:
                continue
            if text:
                return _signature_fromstr(sigcls, base, text)

        if type not in obj.__mro__:
            obj_init, obj_new = obj.__init__, obj.__new__
            if follow_wrapper_chains:
                # e.g. a __new__ wrapped by @warnings.deprecated
                obj_init, obj_new = unwrap(obj_init), unwrap(obj_new)
            if obj_init is object.__init__ and obj_new is object.__new__:
                return sigcls.from_callable(object)
            raise ValueError('no signature found for builtin type {!r}'.format(obj))

    elif not isinstance(obj, _NonUserDefinedCallables):
        call = getattr_static(type(obj), '__call__', None)
        if call is not None:
            try:
                text = obj.__text_signature__
            except AttributeError:
                pass
            else:
                if text:
                    return _signature_fromstr(sigcls, obj, text)
            return recurse(_descriptor_get(call, obj))

    raise ValueError('callable {!r} is not supported by signature'.format(obj))


class _void:
    """Marker for "argument not given" in replace() methods."""


class _empty:
    """Marker object for Signature.empty and Parameter.empty."""


class _ParameterKind(enum.IntEnum):
    POSITIONAL_ONLY = 0
    POSITIONAL_OR_KEYWORD = 1
    VAR_POSITIONAL = 2
    KEYWORD_ONLY = 3
    VAR_KEYWORD = 4

    def __str__(self):
        return self._name_

    @property
    def description(self):
        return _KIND_DESCRIPTIONS[self._value_]

_KIND_DESCRIPTIONS = ('positional-only', 'positional or keyword',
                      'variadic positional', 'keyword-only',
                      'variadic keyword')

_POSITIONAL_ONLY = _ParameterKind.POSITIONAL_ONLY
_POSITIONAL_OR_KEYWORD = _ParameterKind.POSITIONAL_OR_KEYWORD
_VAR_POSITIONAL = _ParameterKind.VAR_POSITIONAL
_KEYWORD_ONLY = _ParameterKind.KEYWORD_ONLY
_VAR_KEYWORD = _ParameterKind.VAR_KEYWORD


class Parameter:
    """One parameter of a Signature: name, kind, default and annotation.

    Parameter objects are immutable; use replace() to derive new ones.
    """

    __slots__ = ('_name', '_kind', '_default', '_annotation')

    POSITIONAL_ONLY = _POSITIONAL_ONLY
    POSITIONAL_OR_KEYWORD = _POSITIONAL_OR_KEYWORD
    VAR_POSITIONAL = _VAR_POSITIONAL
    KEYWORD_ONLY = _KEYWORD_ONLY
    VAR_KEYWORD = _VAR_KEYWORD

    empty = _empty

    def __init__(self, name, kind, *, default=_empty, annotation=_empty):
        try:
            self._kind = _ParameterKind(kind)
        except ValueError:
            raise ValueError(f'value {kind!r} is not a valid Parameter.kind')
        if default is not _empty and self._kind in (_VAR_POSITIONAL, _VAR_KEYWORD):
            raise ValueError(f'{self._kind.description} parameters '
                             f'cannot have default values')
        self._default = default
        self._annotation = annotation

        if name is _empty:
            raise ValueError('name is a required attribute for Parameter')
        if not isinstance(name, str):
            raise TypeError(f'name must be a str, not a {type(name).__name__}')
        if name[:1] == '.' and name[1:].isdigit():
            # Implicit argument of comprehension code objects (".0").
            if self._kind != _POSITIONAL_OR_KEYWORD:
                raise ValueError('implicit arguments must be passed as '
                                 'positional or keyword arguments, not '
                                 f'{self._kind.description}')
            self._kind = _POSITIONAL_ONLY
            name = 'implicit{}'.format(name[1:])
        if not name.isidentifier() or iskeyword(name):
            raise ValueError(f'{name!r} is not a valid parameter name')
        self._name = name

    def __reduce__(self):
        return (type(self), (self._name, self._kind),
                {'_default': self._default, '_annotation': self._annotation})

    def __setstate__(self, state):
        self._default = state['_default']
        self._annotation = state['_annotation']

    @property
    def name(self):
        return self._name

    @property
    def default(self):
        return self._default

    @property
    def annotation(self):
        return self._annotation

    @property
    def kind(self):
        return self._kind

    def replace(self, *, name=_void, kind=_void,
                annotation=_void, default=_void):
        """A copy with the given fields changed."""
        if name is _void:
            name = self._name
        if kind is _void:
            kind = self._kind
        if annotation is _void:
            annotation = self._annotation
        if default is _void:
            default = self._default
        return type(self)(name, kind, default=default, annotation=annotation)

    __replace__ = replace

    def _format(self, *, quote_annotation_strings=True):
        text = self._name
        if self._annotation is not _empty:
            text = '{}: {}'.format(text, formatannotation(
                self._annotation,
                quote_annotation_strings=quote_annotation_strings))
        if self._default is not _empty:
            joiner = ' = ' if self._annotation is not _empty else '='
            text = text + joiner + repr(self._default)
        if self._kind == _VAR_POSITIONAL:
            text = '*' + text
        elif self._kind == _VAR_KEYWORD:
            text = '**' + text
        return text

    def __str__(self):
        return self._format()

    def __repr__(self):
        return '<{} "{}">'.format(self.__class__.__name__, self)

    def __hash__(self):
        return hash((self._name, self._kind, self._annotation, self._default))

    def __eq__(self, other):
        if self is other:
            return True
        if not isinstance(other, Parameter):
            return NotImplemented
        return (self._name == other._name and
                self._kind == other._kind and
                self._default == other._default and
                self._annotation == other._annotation)


class BoundArguments:
    """Result of Signature.bind(): the mapping of parameter names to
    argument values, plus args/kwargs views for calling the function."""

    __slots__ = ('arguments', '_signature', '__weakref__')

    def __init__(self, signature, arguments):
        self.arguments = arguments
        self._signature = signature

    @property
    def signature(self):
        return self._signature

    @property
    def args(self):
        result = []
        for name, param in self._signature.parameters.items():
            if param.kind in (_VAR_KEYWORD, _KEYWORD_ONLY):
                break
            if name not in self.arguments:
                # Later ones can only be passed by keyword.
                break
            value = self.arguments[name]
            if param.kind == _VAR_POSITIONAL:
                result.extend(value)
            else:
                result.append(value)
        return tuple(result)

    @property
    def kwargs(self):
        result = {}
        by_keyword = False
        for name, param in self._signature.parameters.items():
            if not by_keyword:
                if param.kind in (_VAR_KEYWORD, _KEYWORD_ONLY):
                    by_keyword = True
                elif name not in self.arguments:
                    by_keyword = True
                    continue
            if not by_keyword or name not in self.arguments:
                continue
            value = self.arguments[name]
            if param.kind == _VAR_KEYWORD:
                result.update(value)
            else:
                result[name] = value
        return result

    def apply_defaults(self):
        """Fill in default values for missing arguments (empty tuple /
        dict for *args / **kwargs), keeping parameter order."""
        current = self.arguments
        filled = []
        for name, param in self._signature.parameters.items():
            if name in current:
                filled.append((name, current[name]))
            elif param.default is not _empty:
                filled.append((name, param.default))
            elif param.kind is _VAR_POSITIONAL:
                filled.append((name, ()))
            elif param.kind is _VAR_KEYWORD:
                filled.append((name, {}))
        self.arguments = dict(filled)

    def __eq__(self, other):
        if self is other:
            return True
        if not isinstance(other, BoundArguments):
            return NotImplemented
        return (self.signature == other.signature and
                self.arguments == other.arguments)

    __hash__ = None

    def __setstate__(self, state):
        self._signature = state['_signature']
        self.arguments = state['arguments']

    def __getstate__(self):
        return {'_signature': self._signature, 'arguments': self.arguments}

    def __repr__(self):
        parts = ['{}={!r}'.format(name, value)
                 for name, value in self.arguments.items()]
        return '<{} ({})>'.format(self.__class__.__name__, ', '.join(parts))


class Signature:
    """The call signature of a callable: an ordered mapping of
    Parameter objects plus a return annotation.

    Obtain one with signature(callable); Signature objects are immutable.
    """

    __slots__ = ('_return_annotation', '_parameters')

    _parameter_cls = Parameter
    _bound_arguments_cls = BoundArguments

    empty = _empty

    def __init__(self, parameters=None, *, return_annotation=_empty,
                 __validate_parameters__=True):
        params = collections.OrderedDict()
        if parameters is not None:
            if __validate_parameters__:
                self._validate_into(params, parameters)
            else:
                for param in parameters:
                    params[param.name] = param
        self._parameters = types.MappingProxyType(params)
        self._return_annotation = return_annotation

    @staticmethod
    def _validate_into(params, parameters):
        current_kind = _POSITIONAL_ONLY
        seen_default = False
        seen_variadic = set()
        for param in parameters:
            kind = param.kind
            name = param.name
            if kind in (_VAR_POSITIONAL, _VAR_KEYWORD):
                if kind in seen_variadic:
                    raise ValueError(f'more than one {kind.description} parameter')
                seen_variadic.add(kind)
            if kind < current_kind:
                raise ValueError(
                    'wrong parameter order: {} parameter before {} '
                    'parameter'.format(current_kind.description,
                                       kind.description))
            current_kind = kind
            if kind in (_POSITIONAL_ONLY, _POSITIONAL_OR_KEYWORD):
                if param.default is _empty:
                    if seen_default:
                        raise ValueError('non-default argument {!r} follows '
                                         'default argument'.format(name))
                else:
                    seen_default = True
            if name in params:
                raise ValueError('duplicate parameter name: {!r}'.format(name))
            params[name] = param

    @classmethod
    def from_callable(cls, obj, *,
                      follow_wrapped=True, globals=None, locals=None,
                      eval_str=False, annotation_format=Format.VALUE):
        """Signature of the callable *obj*."""
        return _signature_from_callable(obj, sigcls=cls,
                                        follow_wrapper_chains=follow_wrapped,
                                        globals=globals, locals=locals,
                                        eval_str=eval_str,
                                        annotation_format=annotation_format)

    @property
    def parameters(self):
        return self._parameters

    @property
    def return_annotation(self):
        return self._return_annotation

    def replace(self, *, parameters=_void, return_annotation=_void):
        """A copy with new parameters and/or return annotation."""
        if parameters is _void:
            parameters = self.parameters.values()
        if return_annotation is _void:
            return_annotation = self._return_annotation
        return type(self)(parameters, return_annotation=return_annotation)

    __replace__ = replace

    def _hash_basis(self):
        positional = tuple(param for param in self.parameters.values()
                           if param.kind != _KEYWORD_ONLY)
        keyword = {param.name: param for param in self.parameters.values()
                   if param.kind == _KEYWORD_ONLY}
        return positional, keyword, self.return_annotation

    def __hash__(self):
        positional, keyword, ret = self._hash_basis()
        return hash((positional, frozenset(keyword.values()), ret))

    def __eq__(self, other):
        if self is other:
            return True
        if not isinstance(other, Signature):
            return NotImplemented
        return self._hash_basis() == other._hash_basis()

    def _bind(self, args, kwargs, *, partial=False):
        arguments = {}
        params = iter(self.parameters.values())
        pending = ()
        values = iter(args)
        posonly_given_by_keyword = []

        while True:
            try:
                value = next(values)
            except StopIteration:
                # Positional arguments exhausted.
                try:
                    param = next(params)
                except StopIteration:
                    break
                if param.kind == _VAR_POSITIONAL:
                    break
                if param.name in kwargs:
                    if param.kind == _POSITIONAL_ONLY:
                        if param.default is _empty and not partial:
                            raise TypeError(
                                'missing a required positional-only argument: '
                                '{!r}'.format(param.name))
                        posonly_given_by_keyword.append(param)
                        continue
                    pending = (param,)
                    break
                if param.kind == _VAR_KEYWORD or param.default is not _empty:
                    pending = (param,)
                    break
                if partial:
                    pending = (param,)
                    break
                kind = (' positional-only' if param.kind == _POSITIONAL_ONLY
                        else ' keyword-only' if param.kind == _KEYWORD_ONLY
                        else '')
                raise TypeError('missing a required{} argument: {!r}'.format(
                    kind, param.name))
            else:
                try:
                    param = next(params)
                except StopIteration:
                    raise TypeError('too many positional arguments') from None
                if param.kind in (_VAR_KEYWORD, _KEYWORD_ONLY):
                    raise TypeError('too many positional arguments') from None
                if param.kind == _VAR_POSITIONAL:
                    arguments[param.name] = (value,) + tuple(values)
                    break
                if param.name in kwargs and param.kind != _POSITIONAL_ONLY:
                    raise TypeError('multiple values for argument {!r}'.format(
                        param.name)) from None
                arguments[param.name] = value

        var_keyword = None
        for param in itertools.chain(pending, params):
            if param.kind == _VAR_KEYWORD:
                var_keyword = param
                continue
            if param.kind == _VAR_POSITIONAL:
                continue
            if param.kind == _POSITIONAL_ONLY:
                if param.name in kwargs:
                    posonly_given_by_keyword.append(param)
                elif param.default is _empty and not partial:
                    raise TypeError('missing a required positional-only '
                                    'argument: {!r}'.format(param.name))
                continue
            try:
                value = kwargs.pop(param.name)
            except KeyError:
                if not partial and param.default is _empty:
                    raise TypeError('missing a required argument: {!r}'.format(
                        param.name)) from None
            else:
                arguments[param.name] = value

        if kwargs:
            if var_keyword is not None:
                arguments[var_keyword.name] = kwargs
            elif posonly_given_by_keyword:
                names = ', '.join(param.name for param in posonly_given_by_keyword)
                raise TypeError('got some positional-only arguments passed as '
                                'keyword arguments: {!r}'.format(names))
            else:
                raise TypeError('got an unexpected keyword argument {!r}'.format(
                    next(iter(kwargs))))

        return self._bound_arguments_cls(self, arguments)

    def bind(self, /, *args, **kwargs):
        """Map the arguments to parameters like a call would; raise
        TypeError if they do not fit."""
        return self._bind(args, kwargs)

    def bind_partial(self, /, *args, **kwargs):
        """Like bind() but required arguments may be missing."""
        return self._bind(args, kwargs, partial=True)

    def __reduce__(self):
        return (type(self), (tuple(self._parameters.values()),),
                {'_return_annotation': self._return_annotation})

    def __setstate__(self, state):
        self._return_annotation = state['_return_annotation']

    def __repr__(self):
        return '<{} {}>'.format(self.__class__.__name__, self)

    def __str__(self):
        return self.format()

    def format(self, *, max_width=None, quote_annotation_strings=True):
        """Render as "(a, /, b=1, *args, c, **kw) -> ret".

        If max_width is given and the one-line form is longer, one
        parameter per line is used instead.
        """
        parts = []
        pending_slash = False
        need_star = True
        for param in self.parameters.values():
            text = param._format(quote_annotation_strings=quote_annotation_strings)
            kind = param.kind
            if kind == _POSITIONAL_ONLY:
                pending_slash = True
            elif pending_slash:
                parts.append('/')
                pending_slash = False
            if kind == _VAR_POSITIONAL:
                need_star = False
            elif kind == _KEYWORD_ONLY and need_star:
                parts.append('*')
                need_star = False
            parts.append(text)
        if pending_slash:
            parts.append('/')

        rendered = '({})'.format(', '.join(parts))
        if max_width is not None and len(rendered) > max_width:
            rendered = '(\n    {}\n)'.format(',\n    '.join(parts))
        if self.return_annotation is not _empty:
            rendered += ' -> {}'.format(formatannotation(
                self.return_annotation,
                quote_annotation_strings=quote_annotation_strings))
        return rendered


def signature(obj, *, follow_wrapped=True, globals=None, locals=None,
              eval_str=False, annotation_format=Format.VALUE):
    """Return a Signature object for the callable *obj*."""
    return Signature.from_callable(obj, follow_wrapped=follow_wrapped,
                                   globals=globals, locals=locals,
                                   eval_str=eval_str,
                                   annotation_format=annotation_format)


# --------------------------------------------------------------------------
# Command line interface: python -m inspect [--details] module[:qualname]

_CLI_BUILTIN_ERROR = "No source code available for builtin module"
_CLI_NO_SOURCE_ERROR = "No source code available for defining module"
_CLI_NO_TARGET_SOURCE_ERROR = "Failed to retrieve source code for given target"


def _get_details_for_cli(module, nominal_target, target):
    """Information shown by "python -m inspect --details"."""
    if ismodule(target):
        defining = target
        name = target.__name__
    else:
        defining = getmodule(target) or module
        qualname = getattr(target, "__qualname__", None)
        # named after the module object it lives in now (a frozen module may
        # have been renamed since the target was defined)
        modname = getattr(defining, "__name__", None)
        if isinstance(qualname, str) and isinstance(modname, str):
            name = f"{modname}:{qualname}"
        else:
            name = nominal_target
    spec = getattr(defining, '__spec__', None)
    details = {'target': name}
    if name != nominal_target:
        details['alias'] = nominal_target
    details['origin'] = getattr(spec, 'origin', None)
    try:
        source = getsourcefile(defining)
    except Exception:
        source = None
    if source and source.startswith('<frozen'):
        source = getattr(defining, '__file__', None)
    details['source'] = source
    details['cached'] = getattr(spec, 'cached', None)
    if ismodule(target):
        details['loader'] = repr(getattr(spec, 'loader', None))
        if hasattr(target, '__path__'):
            details['submodule_paths'] = repr(target.__path__)

    error = None
    if defining.__name__ in sys.builtin_module_names:
        error = _CLI_BUILTIN_ERROR
    elif source is None:
        error = _CLI_NO_SOURCE_ERROR
    elif not ismodule(target):
        try:
            details['lineno'] = findsource(target)[1]
        except Exception:
            error = _CLI_NO_TARGET_SOURCE_ERROR
    if error is not None:
        details['error'] = error
    return details


def _main():
    """Entry point of "python -m inspect"."""
    import argparse
    import importlib

    parser = argparse.ArgumentParser()
    parser.add_argument(
        'object',
        help="The object to be analysed. "
             "It supports the 'module:qualname' syntax")
    parser.add_argument(
        '-d', '--details', action='store_true',
        help='Display info about the module rather than its source code')
    options = parser.parse_args()

    requested = options.object
    module_name, has_attrs, attrs = requested.partition(":")
    try:
        obj = module = importlib.import_module(module_name)
    except Exception as exc:
        print("Failed to import {} ({}: {})".format(
            module_name, type(exc).__name__, exc), file=sys.stderr)
        sys.exit(2)
    if has_attrs:
        for part in attrs.split("."):
            obj = getattr(obj, part)

    details = _get_details_for_cli(module, requested, obj)
    if options.details:
        line = 'Target: {}'.format(details['target'])
        if 'alias' in details:
            line += ' (looked up as "{}")'.format(details['alias'])
        print(line)
        print('Origin: {}'.format(details['origin']))
        print('Source: {}'.format(details['source']))
        print('Cached: {}'.format(details['cached']))
        if ismodule(obj):
            print('Loader: {}'.format(details['loader']))
            if 'submodule_paths' in details:
                print('Submodule search paths: {}'.format(details['submodule_paths']))
        elif 'error' in details:
            print(details['error'])
        else:
            print('Line: {}'.format(details['lineno']))
        print()
        sys.exit(0)

    if 'error' in details:
        print(details['error'], file=sys.stderr)
        sys.exit(1)
    try:
        print(getsource(obj))
    except (OSError, TypeError):
        print(_CLI_NO_TARGET_SOURCE_ERROR, file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    _main()
