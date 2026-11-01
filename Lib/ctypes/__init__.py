"""create and manipulate C data types in Python"""

import os as _os
import sys as _sys
import struct as _struct

import _ctypes
from _ctypes import (Union, Structure, Array, _Pointer, CFuncPtr as _CFuncPtr,
                     _SimpleCData, CField, ArgumentError,
                     byref, addressof, alignment, sizeof, resize,
                     get_errno, set_errno, buffer_info as _buffer_info,
                     RTLD_LOCAL, RTLD_GLOBAL, SIZEOF_TIME_T,
                     FUNCFLAG_CDECL as _FUNCFLAG_CDECL,
                     FUNCFLAG_PYTHONAPI as _FUNCFLAG_PYTHONAPI,
                     FUNCFLAG_USE_ERRNO as _FUNCFLAG_USE_ERRNO,
                     FUNCFLAG_USE_LASTERROR as _FUNCFLAG_USE_LASTERROR)
from _ctypes import (_memmove_addr, _memset_addr, _string_at_addr,
                     _wstring_at_addr, _cast_addr, _memoryview_at_addr)

DEFAULT_MODE = RTLD_LOCAL


def __getattr__(name):
    if name == "__version__":
        import warnings
        warnings._deprecated("__version__", remove=(3, 20))
        return "1.1.0"
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


# --------------------------------------------------------------- buffers

def create_string_buffer(init, size=None):
    """create_string_buffer(aBytes) -> character array
    create_string_buffer(anInteger) -> character array
    create_string_buffer(aBytes, anInteger) -> character array
    """
    if isinstance(init, bytes):
        if size is None:
            size = len(init) + 1
        _sys.audit("ctypes.create_string_buffer", init, size)
        buf = (c_char * size)()
        buf.value = init
        return buf
    if isinstance(init, int):
        _sys.audit("ctypes.create_string_buffer", None, init)
        return (c_char * init)()
    raise TypeError(init)


def create_unicode_buffer(init, size=None):
    """create_unicode_buffer(aString) -> character array
    create_unicode_buffer(anInteger) -> character array
    create_unicode_buffer(aString, anInteger) -> character array
    """
    if isinstance(init, str):
        if size is None:
            if sizeof(c_wchar) == 2:
                # surrogate pairs need two units
                size = sum(2 if ord(ch) > 0xFFFF else 1 for ch in init) + 1
            else:
                size = len(init) + 1
        _sys.audit("ctypes.create_unicode_buffer", init, size)
        buf = (c_wchar * size)()
        buf.value = init
        return buf
    if isinstance(init, int):
        _sys.audit("ctypes.create_unicode_buffer", None, init)
        return (c_wchar * init)()
    raise TypeError(init)


# --------------------------------------------------------------- function prototypes

_c_functype_cache = {}


def CFUNCTYPE(restype, *argtypes, **kw):
    """CFUNCTYPE(restype, *argtypes,
                 use_errno=False, use_last_error=False) -> function prototype.

    restype: the result type
    argtypes: a sequence specifying the argument types

    The function prototype can be called in different ways to create a
    callable object:

    prototype(integer address) -> foreign function
    prototype(callable) -> create and return a C callable function from callable
    prototype(integer index, method name[, paramflags]) -> foreign function calling a COM method
    prototype((ordinal number, dll object)[, paramflags]) -> foreign function exported by ordinal
    prototype((function name, dll object)[, paramflags]) -> foreign function exported by name
    """
    flags = _FUNCFLAG_CDECL
    if kw.pop("use_errno", False):
        flags |= _FUNCFLAG_USE_ERRNO
    if kw.pop("use_last_error", False):
        flags |= _FUNCFLAG_USE_LASTERROR
    if kw:
        raise ValueError("unexpected keyword argument(s) %s" % kw.keys())
    key = (restype, argtypes, flags)
    try:
        return _c_functype_cache[key]
    except KeyError:
        pass
    ns = {"_argtypes_": argtypes, "_restype_": restype, "_flags_": flags}
    proto = type(_CFuncPtr)("CFunctionType", (_CFuncPtr,), ns)
    _c_functype_cache[key] = proto
    return proto


def PYFUNCTYPE(restype, *argtypes):
    ns = {"_argtypes_": argtypes, "_restype_": restype,
          "_flags_": _FUNCFLAG_CDECL | _FUNCFLAG_PYTHONAPI}
    return type(_CFuncPtr)("CFunctionType", (_CFuncPtr,), ns)


if _os.name == "nt":
    from _ctypes import FUNCFLAG_STDCALL as _FUNCFLAG_STDCALL
    _win_functype_cache = {}

    def WINFUNCTYPE(restype, *argtypes, **kw):
        flags = _FUNCFLAG_STDCALL
        if kw.pop("use_errno", False):
            flags |= _FUNCFLAG_USE_ERRNO
        if kw.pop("use_last_error", False):
            flags |= _FUNCFLAG_USE_LASTERROR
        if kw:
            raise ValueError("unexpected keyword argument(s) %s" % kw.keys())
        key = (restype, argtypes, flags)
        try:
            return _win_functype_cache[key]
        except KeyError:
            pass
        ns = {"_argtypes_": argtypes, "_restype_": restype, "_flags_": flags}
        proto = type(_CFuncPtr)("WinFunctionType", (_CFuncPtr,), ns)
        _win_functype_cache[key] = proto
        return proto


# --------------------------------------------------------------- simple types

def _check_size(typ, typecode=None):
    # the C type must have the size the struct module reports for its code
    if typecode is None:
        typecode = typ._type_
    actual = sizeof(typ)
    required = _struct.calcsize(typecode)
    if actual != required:
        raise SystemError("sizeof(%s) wrong: %d instead of %d" %
                          (typ, actual, required))


class py_object(_SimpleCData):
    _type_ = "O"

    def __repr__(self):
        try:
            return super().__repr__()
        except ValueError:
            return "%s(<NULL>)" % type(self).__name__

_check_size(py_object, "P")


class c_short(_SimpleCData):
    _type_ = "h"
_check_size(c_short)


class c_ushort(_SimpleCData):
    _type_ = "H"
_check_size(c_ushort)


class c_long(_SimpleCData):
    _type_ = "l"
_check_size(c_long)


class c_ulong(_SimpleCData):
    _type_ = "L"
_check_size(c_ulong)


if _struct.calcsize("i") == _struct.calcsize("l"):
    c_int = c_long
    c_uint = c_ulong
else:
    class c_int(_SimpleCData):
        _type_ = "i"
    _check_size(c_int)

    class c_uint(_SimpleCData):
        _type_ = "I"
    _check_size(c_uint)


class c_float(_SimpleCData):
    _type_ = "f"
_check_size(c_float)


class c_double(_SimpleCData):
    _type_ = "d"
_check_size(c_double)


class c_longdouble(_SimpleCData):
    _type_ = "g"
if sizeof(c_longdouble) == sizeof(c_double):
    c_longdouble = c_double


if _struct.calcsize("l") == _struct.calcsize("q"):
    c_longlong = c_long
    c_ulonglong = c_ulong
else:
    class c_longlong(_SimpleCData):
        _type_ = "q"
    _check_size(c_longlong)

    class c_ulonglong(_SimpleCData):
        _type_ = "Q"
    _check_size(c_ulonglong)


class c_ubyte(_SimpleCData):
    _type_ = "B"
_check_size(c_ubyte)


class c_byte(_SimpleCData):
    _type_ = "b"
_check_size(c_byte)


class c_char(_SimpleCData):
    _type_ = "c"
_check_size(c_char)


class c_char_p(_SimpleCData):
    _type_ = "z"

    def __repr__(self):
        return "%s(%s)" % (type(self).__name__, c_void_p.from_buffer(self).value)
_check_size(c_char_p, "P")


class c_void_p(_SimpleCData):
    _type_ = "P"
c_voidp = c_void_p  # backwards compatibility (to a bug)
_check_size(c_void_p)


class c_bool(_SimpleCData):
    _type_ = "?"


class c_wchar_p(_SimpleCData):
    _type_ = "Z"

    def __repr__(self):
        return "%s(%s)" % (type(self).__name__, c_void_p.from_buffer(self).value)


class c_wchar(_SimpleCData):
    _type_ = "u"


if getattr(_ctypes, "_ffi_available", True):
    class c_double_complex(_SimpleCData):
        _type_ = "Zd"
    _check_size(c_double_complex)

    class c_float_complex(_SimpleCData):
        _type_ = "Zf"
    _check_size(c_float_complex)

    class c_longdouble_complex(_SimpleCData):
        _type_ = "Zg"


# fixed-size integer aliases
def _int_of_size(size, signed):
    candidates = ((c_byte, c_short, c_int, c_long, c_longlong) if signed
                  else (c_ubyte, c_ushort, c_uint, c_ulong, c_ulonglong))
    for t in candidates:
        if sizeof(t) == size:
            return t
    raise SystemError("no %d byte integer type" % size)

c_int8 = c_byte
c_uint8 = c_ubyte
c_int16 = _int_of_size(2, True)
c_uint16 = _int_of_size(2, False)
c_int32 = _int_of_size(4, True)
c_uint32 = _int_of_size(4, False)
c_int64 = _int_of_size(8, True)
c_uint64 = _int_of_size(8, False)

c_size_t = _int_of_size(sizeof(c_void_p), False)
c_ssize_t = _int_of_size(sizeof(c_void_p), True)
c_time_t = _int_of_size(SIZEOF_TIME_T, True)


# --------------------------------------------------------------- pointers

class _PointerTypeCache:
    """Deprecated mapping view of the pointer types: ctypes types keep their
    pointer type in ``__pointer_type__``; other keys live in a fallback dict."""

    def _warn(self):
        import warnings
        warnings._deprecated("ctypes._pointer_type_cache", remove=(3, 19))

    def __getitem__(self, key):
        self._warn()
        if isinstance(key, type):
            try:
                return key.__pointer_type__
            except AttributeError:
                pass
        return _pointer_type_cache_fallback[key]

    def __setitem__(self, key, value):
        self._warn()
        if isinstance(key, type):
            key.__pointer_type__ = value
        else:
            _pointer_type_cache_fallback[key] = value

    def __contains__(self, key):
        if isinstance(key, type):
            return hasattr(key, "__pointer_type__")
        return key in _pointer_type_cache_fallback

    def get(self, key, default=None):
        try:
            return self[key]
        except KeyError:
            return default


_pointer_type_cache_fallback = {}
_pointer_type_cache = _PointerTypeCache()


def POINTER(cls):
    """Create and return a new ctypes pointer type.

    Pointer types are cached and reused internally,
    so calling this function repeatedly is cheap.
    """
    if cls is None:
        return c_void_p
    try:
        return cls.__pointer_type__
    except AttributeError:
        pass
    if isinstance(cls, str):
        import warnings
        warnings._deprecated("ctypes.POINTER with string", remove=(3, 19))
        try:
            return _pointer_type_cache_fallback[cls]
        except KeyError:
            result = type(_Pointer)(f"LP_{cls}", (_Pointer,), {})
            _pointer_type_cache_fallback[cls] = result
            return result
    # the metaclass stores the new type as cls.__pointer_type__
    return type(_Pointer)(f"LP_{cls.__name__}", (_Pointer,), {"_type_": cls})


def pointer(obj):
    """Create a new pointer instance, pointing to 'obj'.

    The returned object is of the type POINTER(type(obj)). Note that if you
    just want to pass a pointer to an object to a foreign function call, you
    should use byref(obj) which is much faster.
    """
    return POINTER(type(obj))(obj)


def ARRAY(typ, len):
    return typ * len


def _reset_cache():
    _pointer_type_cache_fallback.clear()
    _c_functype_cache.clear()
    if _os.name == "nt":
        _win_functype_cache.clear()
    # character pointers accept the same arguments as the string types
    POINTER(c_wchar).from_param = c_wchar_p.from_param
    POINTER(c_char).from_param = c_char_p.from_param


# --------------------------------------------------------------- shared libraries

class CDLL:
    """An instance of this class represents a loaded dll/shared
    library, exporting functions using the standard C calling
    convention (named 'cdecl' on Windows).

    The exported functions can be accessed as attributes, or by
    indexing with the function name.  Examples:

    <obj>.qsort -> callable object
    <obj>['qsort'] -> callable object

    Calling the functions releases the Python GIL during the call and
    reacquires it afterwards.
    """
    _func_flags_ = _FUNCFLAG_CDECL
    _func_restype_ = None  # set to c_int below
    _name = "<uninitialized>"
    _handle = 0
    _FuncPtr = None

    def __init__(self, name, mode=DEFAULT_MODE, handle=None,
                 use_errno=False, use_last_error=False, winmode=None):
        if name:
            name = _os.fspath(name)
        self._name = name
        flags = self._func_flags_
        if use_errno:
            flags |= _FUNCFLAG_USE_ERRNO
        if use_last_error:
            flags |= _FUNCFLAG_USE_LASTERROR
        if _sys.platform.startswith("aix"):
            if name and name.endswith(")") and ".a(" in name:
                mode |= (_os.RTLD_MEMBER | _os.RTLD_NOW)

        class _FuncPtr(_CFuncPtr):
            _flags_ = flags
            _restype_ = self._func_restype_
        self._FuncPtr = _FuncPtr

        if handle is None:
            self._handle = _ctypes.dlopen(self._name, mode)
        else:
            self._handle = handle

    def __repr__(self):
        return "<%s '%s', handle %x at %#x>" % (
            type(self).__name__, self._name,
            self._handle & (_sys.maxsize * 2 + 1), id(self) & (_sys.maxsize * 2 + 1))

    def __getattr__(self, name):
        if name.startswith("__") and name.endswith("__"):
            raise AttributeError(name)
        func = self.__getitem__(name)
        setattr(self, name, func)
        return func

    def __getitem__(self, name_or_ordinal):
        func = self._FuncPtr((name_or_ordinal, self))
        if not isinstance(name_or_ordinal, int):
            func.__name__ = name_or_ordinal
        return func


CDLL._func_restype_ = c_int


class PyDLL(CDLL):
    """This class represents the Python library itself.  It allows
    accessing Python API functions.  The GIL is not released, and
    Python exceptions are handled correctly.
    """
    _func_flags_ = _FUNCFLAG_CDECL | _FUNCFLAG_PYTHONAPI


if _os.name == "nt":
    class WinDLL(CDLL):
        """This class represents a dll exporting functions using the
        Windows stdcall calling convention.
        """
        _func_flags_ = _FUNCFLAG_STDCALL


class LibraryLoader:
    def __init__(self, dlltype):
        self._dlltype = dlltype

    def __getattr__(self, name):
        if name[0] == "_":
            raise AttributeError(name)
        try:
            dll = self._dlltype(name)
        except OSError:
            raise AttributeError(name)
        setattr(self, name, dll)
        return dll

    def __getitem__(self, name):
        return getattr(self, name)

    def LoadLibrary(self, name):
        return self._dlltype(name)

    __class_getitem__ = classmethod(type(list[int]))


cdll = LibraryLoader(CDLL)
pydll = LibraryLoader(PyDLL)

if _os.name == "nt":
    pythonapi = PyDLL("python dll", None, _sys.dllhandle)
elif _sys.platform in ("android", "cygwin"):
    pythonapi = PyDLL("libpython%d.%d%s.so" % (*_sys.version_info[:2], getattr(_sys, "abiflags", "")))
else:
    pythonapi = PyDLL(None)


# --------------------------------------------------------------- memory helpers

memmove = CFUNCTYPE(c_void_p, c_void_p, c_void_p, c_size_t)(_memmove_addr)
memset = CFUNCTYPE(c_void_p, c_void_p, c_int, c_size_t)(_memset_addr)

_cast = PYFUNCTYPE(py_object, c_void_p, py_object, py_object)(_cast_addr)


def cast(obj, typ):
    return _cast(obj, obj, typ)


_string_at = PYFUNCTYPE(py_object, c_void_p, c_ssize_t)(_string_at_addr)


def string_at(ptr, size=-1):
    """string_at(ptr[, size]) -> string

    Return the byte string at void *ptr."""
    return _string_at(ptr, size)


_wstring_at = PYFUNCTYPE(py_object, c_void_p, c_ssize_t)(_wstring_at_addr)


def wstring_at(ptr, size=-1):
    """wstring_at(ptr[, size]) -> string

    Return the wide-character string at void *ptr."""
    return _wstring_at(ptr, size)


_memoryview_at = PYFUNCTYPE(py_object, c_void_p, c_ssize_t, c_int)(_memoryview_at_addr)


def memoryview_at(ptr, size, readonly=False):
    """memoryview_at(ptr, size[, readonly]) -> memoryview

    Return a memoryview representing the memory at void *ptr."""
    return _memoryview_at(ptr, size, bool(readonly))


if _os.name == "nt":
    def WinError(code=None, descr=None):
        if code is None:
            code = GetLastError()
        if descr is None:
            descr = FormatError(code).strip()
        return OSError(None, descr, None, code)


from ctypes._endian import BigEndianStructure, LittleEndianStructure
from ctypes._endian import BigEndianUnion, LittleEndianUnion

_reset_cache()
