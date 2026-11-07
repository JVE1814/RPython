"""Path-based part of the import system: the path finder and file finders,
loaders for source, bytecode and extension modules, namespace packages, and
the helpers for bytecode cache (.pyc) files.

The interpreter loads this file at start-up as ``_frozen_importlib_external``
right after ``_bootstrap`` and calls ``_install(_bootstrap_module)``.  Only
built-in modules can be imported at that point.
"""

import _imp
import sys

try:
    import _io
except ImportError:
    _io = None
try:
    import marshal
except ImportError:
    marshal = None
try:
    import _warnings
except ImportError:
    _warnings = None


# ---------------------------------------------------------------- operating system access

class _FallbackStat:
    __slots__ = ('st_mode', 'st_size', 'st_mtime')

    def __init__(self, mode, size, mtime):
        self.st_mode = mode
        self.st_size = size
        self.st_mtime = mtime


class _FallbackOS:
    """The few file-system calls this module needs, through _imp, for
    interpreters built without a posix module."""

    @staticmethod
    def stat(path):
        return _FallbackStat(*_imp._rpy_stat(path))

    @staticmethod
    def listdir(path):
        return _imp._rpy_listdir(path)

    @staticmethod
    def getcwd():
        return _imp._rpy_getcwd()

    @staticmethod
    def fspath(path):
        if isinstance(path, (str, bytes)):
            return path
        hook = getattr(type(path), '__fspath__', None)
        if hook is None:
            raise TypeError('expected str, bytes or os.PathLike object, not '
                            + type(path).__name__)
        result = hook(path)
        if not isinstance(result, (str, bytes)):
            raise TypeError(f'expected {type(path).__name__}.__fspath__() to '
                            f'return str or bytes, not {type(result).__name__}')
        return result

    @staticmethod
    def mkdir(path, mode=0o777):
        raise OSError('directory creation not supported')


if 'posix' in sys.builtin_module_names:
    import posix as _os
else:
    _os = _FallbackOS()

# importlib._bootstrap, set by _install() / _set_bootstrap_module()
_bootstrap = None

path_sep = '/'
path_separators = '/'
_pathseps_with_colon = {':' + sep for sep in path_separators}

# Platforms whose file systems usually ignore case; PYTHONCASEOK then asks for
# case-insensitive module lookups (str key on Windows, bytes key elsewhere).
_CASE_INSENSITIVE_PLATFORMS_STR_KEY = 'win',
_CASE_INSENSITIVE_PLATFORMS_BYTES_KEY = 'cygwin', 'darwin', 'ios', 'tvos', 'watchos'
_CASE_INSENSITIVE_PLATFORMS = (_CASE_INSENSITIVE_PLATFORMS_BYTES_KEY
                               + _CASE_INSENSITIVE_PLATFORMS_STR_KEY)


def _warn(message, category, stacklevel=1):
    if _warnings is not None:
        _warnings.warn(message, category, stacklevel + 1)
        return
    try:
        import warnings
    except ImportError:
        return
    warnings.warn(message, category, stacklevel + 1)


def _make_relax_case():
    if not sys.platform.startswith(_CASE_INSENSITIVE_PLATFORMS):
        def _relax_case():
            """Module names are matched case-sensitively here."""
            return False
        return _relax_case
    on_windows = sys.platform.startswith(_CASE_INSENSITIVE_PLATFORMS_STR_KEY)
    key = 'PYTHONCASEOK' if on_windows else b'PYTHONCASEOK'

    def _relax_case():
        """Match module names case-insensitively (PYTHONCASEOK is set and
        the environment is not ignored)?"""
        return not sys.flags.ignore_environment and key in _os.environ
    return _relax_case


_relax_case = _make_relax_case()


def _pack_uint32(x):
    """The low 32 bits of *x* as 4 little-endian bytes."""
    return (int(x) & 0xFFFFFFFF).to_bytes(4, 'little')


def _unpack_uint32(data):
    """4 little-endian bytes as an int."""
    assert len(data) == 4
    return int.from_bytes(data, 'little')


def _unpack_uint16(data):
    """2 little-endian bytes as an int."""
    assert len(data) == 2
    return int.from_bytes(data, 'little')


# ---------------------------------------------------------------- path helpers (no os.path yet)

def _path_join(*path_parts):
    """Join path components with the separator (empty parts are skipped)."""
    if not path_parts:
        return ''
    if len(path_parts) == 1:
        return path_parts[0]
    return path_sep.join(part.rstrip(path_separators) for part in path_parts if part)


def _path_split(path):
    """(head, tail) around the last separator."""
    head, sep, tail = path.rpartition(path_sep)
    return head, tail


def _path_stat(path):
    """os.stat(path); tests replace this function to fake file systems."""
    return _os.stat(path)


def _path_is_mode_type(path, mode):
    try:
        st = _path_stat(path)
    except OSError:
        return False
    return (st.st_mode & 0o170000) == mode


def _path_isfile(path):
    return _path_is_mode_type(path, 0o100000)


def _path_isdir(path):
    return _path_is_mode_type(path or _os.getcwd(), 0o040000)


def _path_isabs(path):
    return path.startswith(path_separators)


def _path_abspath(path):
    """Absolute form of *path* (no normalisation, no file system access
    except for the current directory)."""
    if _path_isabs(path):
        return path
    for sep in path_separators:
        path = path.removeprefix('.' + sep)
    return _path_join(_os.getcwd(), path)


def _write_atomic(path, data, mode=0o666):
    """Write *data* to *path* through a temporary file renamed into place,
    so that a concurrent reader sees either the old or the complete file."""
    tmp_path = f'{path}.{id(path)}'
    fd = _os.open(tmp_path, _os.O_EXCL | _os.O_CREAT | _os.O_WRONLY, mode & 0o666)
    try:
        with _io.FileIO(fd, 'wb') as tmp_file:
            remaining = memoryview(data)
            while remaining:
                written = tmp_file.write(remaining)
                if not written:
                    raise OSError('Short write when writing pyc file')
                remaining = remaining[written:]
        _os.replace(tmp_path, path)
    except OSError:
        try:
            _os.unlink(tmp_path)
        except OSError:
            pass
        raise


_code_type = type(_write_atomic.__code__)


# ---------------------------------------------------------------- bytecode cache files

# The pyc format number (our code objects are not CPython's, so caches written
# by one interpreter are never read by the other).
MAGIC_NUMBER = _imp.pyc_magic_number_token.to_bytes(4, 'little')
_RAW_MAGIC_NUMBER = int.from_bytes(MAGIC_NUMBER, 'little')

_PYCACHE = '__pycache__'
_OPT = 'opt-'

SOURCE_SUFFIXES = ['.py']
BYTECODE_SUFFIXES = ['.pyc']
# filled from _imp.extension_suffixes() at the end of this module
EXTENSION_SUFFIXES = []


def cache_from_source(path, debug_override=None, *, optimization=None):
    """The .pyc path for the source file *path*:
    ``<dir>/__pycache__/<name>.<cache tag>[.opt-<optimization>].pyc``
    (below sys.pycache_prefix when that is set)."""
    if debug_override is not None:
        _warn("the debug_override parameter is deprecated; use 'optimization' instead",
              DeprecationWarning)
        if optimization is not None:
            raise TypeError('debug_override or optimization must be set to None')
        optimization = '' if debug_override else 1
    path = _os.fspath(path)
    head, tail = _path_split(path)
    stem, dot, suffix = tail.rpartition('.')
    tag = sys.implementation.cache_tag
    if tag is None:
        raise NotImplementedError('sys.implementation.cache_tag is None')
    name = f'{stem or suffix}{dot}{tag}'
    if optimization is None:
        optimization = sys.flags.optimize or ''
    optimization = str(optimization)
    if optimization:
        if not optimization.isalnum():
            raise ValueError(f'{optimization!r} is not alphanumeric')
        name = f'{name}.{_OPT}{optimization}'
    filename = name + BYTECODE_SUFFIXES[0]
    prefix = sys.pycache_prefix
    if prefix is None:
        return _path_join(head, _PYCACHE, filename)
    # the source directory, made absolute, is mirrored below the prefix
    if not _path_isabs(head):
        head = _path_join(_os.getcwd(), head)
    return _path_join(prefix, head.lstrip(path_separators), filename)


def source_from_cache(path):
    """The source path a .pyc path from cache_from_source() belongs to."""
    if sys.implementation.cache_tag is None:
        raise NotImplementedError('sys.implementation.cache_tag is None')
    path = _os.fspath(path)
    head, pyc_name = _path_split(path)
    in_prefix = False
    prefix = sys.pycache_prefix
    if prefix is not None:
        prefix = prefix.rstrip(path_separators)
        if head.startswith(prefix + path_sep):
            head = head[len(prefix):]
            in_prefix = True
    if not in_prefix:
        head, cache_dir = _path_split(head)
        if cache_dir != _PYCACHE:
            raise ValueError(f'{_PYCACHE} not bottom-level directory in {path!r}')
    dots = pyc_name.count('.')
    if dots not in (2, 3):
        raise ValueError(f'expected only 2 or 3 dots in {pyc_name!r}')
    if dots == 3:
        opt_part = pyc_name.rsplit('.', 2)[-2]
        if not opt_part.startswith(_OPT):
            raise ValueError(f"optimization portion of filename does not start with {_OPT!r}")
        if not opt_part[len(_OPT):].isalnum():
            raise ValueError(f"optimization level {opt_part!r} is not an alphanumeric value")
    stem = pyc_name.partition('.')[0]
    return _path_join(head, stem + SOURCE_SUFFIXES[0])


def _get_sourcefile(bytecode_path):
    """The source file belonging to a bytecode path when it exists, else the
    bytecode path itself (None for an empty path)."""
    if not bytecode_path:
        return None
    rest, _, extension = bytecode_path.rpartition('.')
    if not rest or extension.lower()[-3:-1] != 'py':
        return bytecode_path
    try:
        source = source_from_cache(bytecode_path)
    except (NotImplementedError, ValueError):
        source = bytecode_path[:-1]
    return source if _path_isfile(source) else bytecode_path


def _get_cached(filename):
    """The cached bytecode location for a module file (None if none)."""
    if filename.endswith(tuple(SOURCE_SUFFIXES)):
        try:
            return cache_from_source(filename)
        except NotImplementedError:
            return None
    if filename.endswith(tuple(BYTECODE_SUFFIXES)):
        return filename
    return None


def _calc_mode(path):
    """Permission bits for the cache file of source *path*: the source's,
    always writable by the owner."""
    try:
        mode = _path_stat(path).st_mode
    except OSError:
        mode = 0o666
    return mode | 0o200


def _check_name(method):
    """Decorator for loader methods taking a module name: the loader only
    serves its own module (None means that one)."""
    def _check_name_wrapper(self, name=None, *args, **kwargs):
        if name is None:
            name = self.name
        elif name != self.name:
            raise ImportError(f'loader for {self.name} cannot handle {name}', name=name)
        return method(self, name, *args, **kwargs)

    for attr in ('__module__', '__name__', '__qualname__', '__doc__'):
        if hasattr(method, attr):
            setattr(_check_name_wrapper, attr, getattr(method, attr))
    _check_name_wrapper.__dict__.update(method.__dict__)
    return _check_name_wrapper


# A .pyc file starts with a 16-byte header: the magic number, a 32-bit flags
# word, then either the source's mtime and size (flags == 0) or an 8-byte hash
# of the source (bit 0 set; bit 1: check the hash against the source).

def _classify_pyc(data, name, exc_details):
    """Validate the magic number and flags of .pyc *data*; return the flags."""
    magic = data[:4]
    if magic != MAGIC_NUMBER:
        message = f'bad magic number in {name!r}: {magic!r}'
        _bootstrap._verbose_message('{}', message)
        raise ImportError(message, **exc_details)
    if len(data) < 16:
        message = f'reached EOF while reading pyc header of {name!r}'
        _bootstrap._verbose_message('{}', message)
        raise EOFError(message)
    flags = _unpack_uint32(data[4:8])
    if flags & ~0b11:
        raise ImportError(f'invalid flags {flags!r} in {name!r}', **exc_details)
    return flags


def _validate_timestamp_pyc(data, source_mtime, source_size, name, exc_details):
    """Raise ImportError when a timestamp-based .pyc does not describe the
    source (by mtime and, when known, size)."""
    stale = _unpack_uint32(data[8:12]) != (int(source_mtime) & 0xFFFFFFFF)
    if stale:
        message = f'bytecode is stale for {name!r}'
        _bootstrap._verbose_message('{}', message)
        raise ImportError(message, **exc_details)
    if source_size is not None and _unpack_uint32(data[12:16]) != (source_size & 0xFFFFFFFF):
        raise ImportError(f'bytecode is stale for {name!r}', **exc_details)


def _validate_hash_pyc(data, source_hash, name, exc_details):
    """Raise ImportError when a hash-based .pyc was made from other source."""
    if data[8:16] != source_hash:
        raise ImportError(f"hash in bytecode doesn't match hash of source {name!r}",
                          **exc_details)


def _compile_bytecode(data, name=None, bytecode_path=None, source_path=None):
    """The code object marshalled in .pyc payload *data*."""
    if marshal is None:
        raise ImportError('marshal is not available', name=name, path=bytecode_path)
    code = marshal.loads(data)
    if not isinstance(code, _code_type):
        raise ImportError(f'Non-code object in {bytecode_path!r}', name=name, path=bytecode_path)
    _bootstrap._verbose_message('code object from {!r}', bytecode_path)
    if source_path is not None:
        _imp._fix_co_filename(code, source_path)
    return code


def _code_to_timestamp_pyc(code, mtime=0, source_size=0):
    """.pyc data (timestamp-based header) for *code*."""
    return bytearray(MAGIC_NUMBER + _pack_uint32(0) + _pack_uint32(mtime)
                     + _pack_uint32(source_size) + marshal.dumps(code))


def _code_to_hash_pyc(code, source_hash, checked=True):
    """.pyc data (hash-based header) for *code*."""
    assert len(source_hash) == 8
    flags = 0b01 | (0b10 if checked else 0)
    return bytearray(MAGIC_NUMBER + _pack_uint32(flags) + source_hash + marshal.dumps(code))


def decode_source(source_bytes):
    """Decode Python source bytes (PEP 263 coding cookie or BOM; UTF-8 by
    default) with universal newlines."""
    try:
        import tokenize
    except ImportError:
        encoding = 'utf-8-sig' if source_bytes.startswith(b'\xef\xbb\xbf') else 'utf-8'
    else:
        encoding = tokenize.detect_encoding(_io.BytesIO(source_bytes).readline)[0]
    text = source_bytes.decode(encoding)
    return text.replace('\r\n', '\n').replace('\r', '\n')


# ---------------------------------------------------------------- specs

# default of spec_from_file_location's submodule_search_locations: ask the loader
_POPULATE = object()


def spec_from_file_location(name, location=None, *, loader=None,
                            submodule_search_locations=_POPULATE):
    """A ModuleSpec for a module loaded from the file *location*.

    Without a loader, the loader is chosen by the file's suffix (None is
    returned for unknown suffixes).  Packages get the directory of the file
    as their search location unless *submodule_search_locations* is given.
    """
    if location is None:
        location = '<unknown>'
        if hasattr(loader, 'get_filename'):
            try:
                location = loader.get_filename(name)
            except ImportError:
                pass
    else:
        location = _os.fspath(location)
        try:
            location = _path_abspath(location)
        except OSError:
            pass

    spec = _bootstrap.ModuleSpec(name, loader, origin=location)
    spec._set_fileattr = True

    if loader is None:
        for loader_class, suffixes in _get_supported_file_loaders():
            if location.endswith(tuple(suffixes)):
                loader = spec.loader = loader_class(name, location)
                break
        else:
            return None

    if submodule_search_locations is not _POPULATE:
        spec.submodule_search_locations = submodule_search_locations
    elif hasattr(loader, 'is_package'):
        try:
            if loader.is_package(name):
                spec.submodule_search_locations = []
        except ImportError:
            pass
    if spec.submodule_search_locations == [] and location:
        spec.submodule_search_locations.append(_path_split(location)[0])
    return spec


def _bless_my_loader(module_globals):
    """The loader to use for the module with *module_globals* (warnings
    module, linecache).  __spec__.loader is authoritative; a differing or
    lone __loader__ is still honoured, with a DeprecationWarning."""
    missing = object()
    loader = module_globals.get('__loader__', None)
    spec = module_globals.get('__spec__', missing)
    if loader is None:
        if spec is missing:
            return None
        if spec is None:
            raise ValueError('Module globals is missing a __spec__.loader')
    spec_loader = getattr(spec, 'loader', missing)
    if spec_loader is missing or spec_loader is None:
        if loader is None:
            error = AttributeError if spec_loader is missing else ValueError
            raise error('Module globals is missing a __spec__.loader')
        _warn('Module globals is missing a __spec__.loader', DeprecationWarning)
        return loader
    if loader is not None and loader != spec_loader:
        _warn('Module globals; __loader__ != __spec__.loader', DeprecationWarning)
        return loader
    return spec_loader


# ---------------------------------------------------------------- loaders

class _LoaderBasics:
    """What the source and bytecode file loaders have in common."""

    def is_package(self, fullname):
        """A module whose file is ``__init__.<suffix>`` is a package."""
        filename = _path_split(self.get_filename(fullname))[1]
        return (filename.rsplit('.', 1)[0] == '__init__'
                and fullname.rpartition('.')[2] != '__init__')

    def create_module(self, spec):
        """Use the default module object."""

    def exec_module(self, module):
        """Run the module's code in its namespace."""
        code = self.get_code(module.__name__)
        if code is None:
            raise ImportError(f'cannot load module {module.__name__!r} when '
                              'get_code() returns None')
        _bootstrap._call_with_frames_removed(exec, code, module.__dict__)

    def load_module(self, fullname):
        """Deprecated: use exec_module()."""
        return _bootstrap._load_module_shim(self, fullname)


class SourceLoader(_LoaderBasics):
    """Base for loaders of Python source that keep a bytecode cache.

    Subclasses provide get_filename() and get_data(), and for caching
    path_stats() (or path_mtime()) and set_data().
    """

    def path_mtime(self, path):
        """Modification time of *path*; OSError means "unknown"."""
        raise OSError

    def path_stats(self, path):
        """{'mtime': ..., optionally 'size': ...} of *path*; OSError means
        "unknown"."""
        return {'mtime': self.path_mtime(path)}

    def _cache_bytecode(self, source_path, cache_path, data):
        """Store .pyc data (subclasses may use the source path, e.g. for
        the file mode)."""
        return self.set_data(cache_path, data)

    def set_data(self, path, data):
        """Write *data* to *path*; the default writes nothing."""

    def get_source(self, fullname):
        """The module's source as str."""
        path = self.get_filename(fullname)
        try:
            raw = self.get_data(path)
        except OSError as exc:
            raise ImportError('source not available through get_data()', name=fullname) from exc
        return decode_source(raw)

    def source_to_code(self, data, path, fullname=None, *, _optimize=-1):
        """Compile source *data* (str or bytes) from *path*."""
        options = {'dont_inherit': True, 'optimize': _optimize}
        if fullname is not None:
            options['module'] = fullname
        return _bootstrap._call_with_frames_removed(compile, data, path, 'exec', **options)

    def _cached_code(self, fullname, source_path, bytecode_path, stats, state):
        """The code from a valid cache file, or None.  *state* collects what
        get_code() needs to write a new cache file."""
        try:
            data = self.get_data(bytecode_path)
        except OSError:
            return None
        details = {'name': fullname, 'path': bytecode_path}
        try:
            flags = _classify_pyc(data, fullname, details)
            if flags & 0b01:
                state['hash_based'] = True
                state['check_source'] = bool(flags & 0b10)
                mode = _imp.check_hash_based_pycs
                if mode != 'never' and (state['check_source'] or mode == 'always'):
                    state['source_bytes'] = self.get_data(source_path)
                    state['source_hash'] = _imp.source_hash(_RAW_MAGIC_NUMBER, state['source_bytes'])
                    _validate_hash_pyc(data, state['source_hash'], fullname, details)
            else:
                _validate_timestamp_pyc(data, stats['mtime'], stats.get('size'), fullname, details)
        except (ImportError, EOFError):
            return None     # stale or broken header: recompile
        _bootstrap._verbose_message('{} matches {}', bytecode_path, source_path)
        # errors in the payload itself are reported (not silently recompiled)
        return _compile_bytecode(data[16:], name=fullname, bytecode_path=bytecode_path,
                                 source_path=source_path)

    def get_code(self, fullname):
        """The module's code object: from a valid cache file when possible,
        otherwise compiled from the source (and cached)."""
        source_path = self.get_filename(fullname)
        state = {'hash_based': False, 'check_source': True,
                 'source_bytes': None, 'source_hash': None}
        source_mtime = None
        try:
            bytecode_path = cache_from_source(source_path)
        except NotImplementedError:
            bytecode_path = None
        if bytecode_path is not None:
            try:
                stats = self.path_stats(source_path)
            except OSError:
                stats = None
            if stats is not None:
                source_mtime = int(stats['mtime'])
                code = self._cached_code(fullname, source_path, bytecode_path, stats, state)
                if code is not None:
                    return code

        source_bytes = state['source_bytes']
        if source_bytes is None:
            source_bytes = self.get_data(source_path)
        code = self.source_to_code(source_bytes, source_path, fullname)
        _bootstrap._verbose_message('code object from {}', source_path)
        if (sys.dont_write_bytecode or bytecode_path is None or source_mtime is None
                or marshal is None):
            return code
        try:
            if state['hash_based']:
                source_hash = state['source_hash']
                if source_hash is None:
                    source_hash = _imp.source_hash(_RAW_MAGIC_NUMBER, source_bytes)
                data = _code_to_hash_pyc(code, source_hash, state['check_source'])
            else:
                data = _code_to_timestamp_pyc(code, source_mtime, len(source_bytes))
        except (ValueError, TypeError):
            return code     # not marshallable: just don't cache it
        try:
            self._cache_bytecode(source_path, bytecode_path, data)
        except NotImplementedError:
            pass
        return code


def _read_file(path):
    if _io is None:
        return _imp._rpy_read_file(_os.fspath(path))
    with _io.open_code(str(path)) as file:
        return file.read()


class FileLoader:
    """Base for loaders of one module stored in one file."""

    def __init__(self, fullname, path):
        self.name = fullname
        self.path = path

    def __eq__(self, other):
        return type(self) is type(other) and self.__dict__ == other.__dict__

    def __hash__(self):
        return hash(self.name) ^ hash(self.path)

    @_check_name
    def load_module(self, fullname):
        """Deprecated: use exec_module()."""
        return super(FileLoader, self).load_module(fullname)

    @_check_name
    def get_filename(self, fullname):
        """The path of the module's file."""
        return self.path

    def get_data(self, path):
        """The bytes of the file at *path* (code files are opened with
        io.open_code())."""
        if _io is None or isinstance(self, (SourceLoader, ExtensionFileLoader)):
            return _read_file(path)
        with _io.FileIO(path, 'r') as file:
            return file.read()

    @_check_name
    def get_resource_reader(self, module):
        from importlib.readers import FileReader
        return FileReader(self)


class SourceFileLoader(FileLoader, SourceLoader):
    """Loader for .py files, caching bytecode in __pycache__."""

    def path_stats(self, path):
        st = _path_stat(path)
        return {'mtime': st.st_mtime, 'size': st.st_size}

    def _cache_bytecode(self, source_path, bytecode_path, data):
        return self.set_data(bytecode_path, data, _mode=_calc_mode(source_path))

    def set_data(self, path, data, *, _mode=0o666):
        """Write a cache file, creating missing directories; failures are
        only reported under -v."""
        if _io is None or isinstance(_os, _FallbackOS):
            return
        directory = _path_split(path)[0]
        missing = []
        while directory and not _path_isdir(directory):
            directory, part = _path_split(directory)
            missing.append(part)
        for part in reversed(missing):
            directory = _path_join(directory, part)
            try:
                _os.mkdir(directory)
            except FileExistsError:
                continue    # created concurrently
            except OSError as exc:
                _bootstrap._verbose_message('could not create {!r}: {!r}', directory, exc)
                return
        try:
            _write_atomic(path, data, _mode)
        except OSError as exc:
            _bootstrap._verbose_message('could not create {!r}: {!r}', path, exc)
            return
        _bootstrap._verbose_message('created {!r}', path)


class SourcelessFileLoader(FileLoader, _LoaderBasics):
    """Loader for .pyc files without source."""

    def get_code(self, fullname):
        path = self.get_filename(fullname)
        data = self.get_data(path)
        _classify_pyc(data, fullname, {'name': fullname, 'path': path})
        return _compile_bytecode(data[16:], name=fullname, bytecode_path=path)

    def get_source(self, fullname):
        """No source exists."""
        return None


class ExtensionFileLoader(FileLoader, _LoaderBasics):
    """Loader for extension modules (shared libraries)."""

    def __init__(self, name, path):
        self.name = name
        self.path = path

    def __eq__(self, other):
        return type(self) is type(other) and self.__dict__ == other.__dict__

    def __hash__(self):
        return hash(self.name) ^ hash(self.path)

    def create_module(self, spec):
        """Run the extension's initialisation function."""
        module = _bootstrap._call_with_frames_removed(_imp.create_dynamic, spec)
        _bootstrap._verbose_message('extension module {!r} loaded from {!r}', spec.name, self.path)
        return module

    def exec_module(self, module):
        """Run the execution slots of a multi-phase extension."""
        _bootstrap._call_with_frames_removed(_imp.exec_dynamic, module)
        _bootstrap._verbose_message('extension module {!r} executed from {!r}', self.name, self.path)

    def is_package(self, fullname):
        """A shared library named __init__.<extension suffix> is a package."""
        filename = _path_split(self.path)[1]
        return any(filename == '__init__' + suffix for suffix in EXTENSION_SUFFIXES)

    def get_code(self, fullname):
        """Extension modules have no code object."""
        return None

    def get_source(self, fullname):
        """Extension modules have no source."""
        return None

    @_check_name
    def get_filename(self, fullname):
        return self.path


class AppleFrameworkLoader(ExtensionFileLoader):
    """Extension loader for Apple's mobile platforms, where the binary lives
    in a Frameworks folder and a ``.fwork`` file next to the module names it.
    Elsewhere it behaves like ExtensionFileLoader."""

    def create_module(self, spec):
        marker = spec.origin
        if marker.endswith('.fwork'):
            with _io.FileIO(marker, 'r') as f:
                relative = f.read().decode().strip()
            bundle = marker.rpartition('/')[0].rpartition('/')[0] if '/' in marker else ''
            spec.origin = _path_join(bundle, relative) if bundle else relative
            self.path = spec.origin
        module = _bootstrap._call_with_frames_removed(_imp.create_dynamic, spec)
        module.__file__ = marker
        return module


# ---------------------------------------------------------------- namespace packages

class _NamespacePath:
    """__path__ of a namespace package.

    It is recomputed (through the path finder) when the parent's path changes
    or caches were invalidated, so portions added later become visible.
    """

    # bumped by PathFinder.invalidate_caches()
    _epoch = 0

    def __init__(self, name, path, path_finder):
        self._name = name
        self._path = path
        self._path_finder = path_finder
        self._last_parent_path = tuple(self._get_parent_path())
        self._last_epoch = self._epoch

    def _find_parent_path_names(self):
        """(module, attribute) holding the parent path."""
        parent, dot, _ = self._name.rpartition('.')
        return (parent, '__path__') if dot else ('sys', 'path')

    def _get_parent_path(self):
        module_name, attr = self._find_parent_path_names()
        try:
            parent = sys.modules[module_name]
        except KeyError:
            # e.g. PathFinder.find_spec('pkg.sub') without importing pkg
            raise ModuleNotFoundError(f'No module named {module_name!r}', name=module_name) from None
        return getattr(parent, attr)

    def _recalculate(self):
        parent_path = tuple(self._get_parent_path())
        if parent_path != self._last_parent_path or self._epoch != self._last_epoch:
            spec = self._path_finder(self._name, parent_path)
            # still a namespace package with portions: take the new portions
            if spec is not None and spec.loader is None and spec.submodule_search_locations:
                self._path = spec.submodule_search_locations
            self._last_parent_path = parent_path
            self._last_epoch = self._epoch
        return self._path

    def __iter__(self):
        return iter(self._recalculate())

    def __getitem__(self, index):
        return self._recalculate()[index]

    def __setitem__(self, index, path):
        self._path[index] = path

    def __len__(self):
        return len(self._recalculate())

    def __repr__(self):
        return f'_NamespacePath({self._path!r})'

    def __contains__(self, item):
        return item in self._recalculate()

    def append(self, item):
        self._path.append(item)


class NamespaceLoader:
    """Loader of namespace packages (directories without __init__)."""

    def __init__(self, name, path, path_finder):
        self._path = _NamespacePath(name, path, path_finder)

    def is_package(self, fullname):
        return True

    def get_source(self, fullname):
        return ''

    def get_code(self, fullname):
        return compile('', '<string>', 'exec', dont_inherit=True)

    def create_module(self, spec):
        """Use the default module object."""

    def exec_module(self, module):
        """Nothing to execute."""

    def load_module(self, fullname):
        """Deprecated: use exec_module()."""
        _bootstrap._verbose_message('namespace module loaded with path {!r}', self._path)
        return _bootstrap._load_module_shim(self, fullname)

    def get_resource_reader(self, module):
        from importlib.readers import NamespaceReader
        return NamespaceReader(self._path)


# older private name, kept for code that used it
_NamespaceLoader = NamespaceLoader


# ---------------------------------------------------------------- finders

def _search_locations(parent):
    """The directories to search for submodules of the package spec
    *parent* (sys.path for top-level modules)."""
    if parent is None:
        return sys.path
    locations = parent.submodule_search_locations
    if locations is None:
        raise ValueError(f'{parent.name!r} is not a package')
    return locations


class PathFinder:
    """Meta path finder for sys.path and package __path__ entries.

    Each path entry gets a path entry finder from sys.path_hooks, cached in
    sys.path_importer_cache.
    """

    @staticmethod
    def invalidate_caches():
        """Forget cached finders of relative or unusable entries and tell
        the others to refresh their caches."""
        cache = sys.path_importer_cache
        for entry, finder in list(cache.items()):
            if finder is None or not _path_isabs(entry):
                del cache[entry]
            elif hasattr(finder, 'invalidate_caches'):
                finder.invalidate_caches()
        _NamespacePath._epoch += 1
        try:
            from importlib.metadata import MetadataPathFinder
        except ImportError:
            return
        MetadataPathFinder.invalidate_caches()

    @staticmethod
    def _path_hooks(path):
        """The first finder a hook in sys.path_hooks makes for *path*."""
        hooks = sys.path_hooks
        if hooks is not None and not hooks:
            _warn('sys.path_hooks is empty', ImportWarning)
        for hook in hooks:
            try:
                return hook(path)
            except ImportError:
                pass
        return None

    @classmethod
    def _path_importer_cache(cls, path):
        """The (cached) path entry finder for *path* ('' = current directory)."""
        if path == '':
            try:
                path = _os.getcwd()
            except (FileNotFoundError, PermissionError):
                return None     # the current directory is gone
        cache = sys.path_importer_cache
        try:
            return cache[path]
        except KeyError:
            finder = cache[path] = cls._path_hooks(path)
            return finder

    @classmethod
    def _get_spec(cls, fullname, path, target=None):
        """Search *path* for *fullname*.  A module found anywhere wins; else
        the portions of a namespace package are collected into a loader-less
        spec (with an empty list when nothing was found)."""
        portions = []
        for entry in path:
            if not isinstance(entry, str):
                continue
            finder = cls._path_importer_cache(entry)
            if finder is None:
                continue
            spec = finder.find_spec(fullname, target)
            if spec is None:
                continue
            if spec.loader is not None:
                return spec
            if spec.submodule_search_locations is None:
                raise ImportError('spec missing loader')
            portions.extend(spec.submodule_search_locations)
        spec = _bootstrap.ModuleSpec(fullname, None)
        spec.submodule_search_locations = portions
        return spec

    @classmethod
    def find_spec(cls, fullname, path=None, target=None):
        """Spec for *fullname* on *path* (default sys.path), or None."""
        spec = cls._get_spec(fullname, sys.path if path is None else path, target)
        if spec is None:
            return None
        if spec.loader is not None:
            return spec
        portions = spec.submodule_search_locations
        if not portions:
            return None
        # a namespace package
        spec.origin = None
        spec.submodule_search_locations = _NamespacePath(fullname, portions, cls._get_spec)
        return spec

    @classmethod
    def discover(cls, parent=None):
        """Yield specs of the modules the path entry finders can find on
        sys.path, or in the package *parent* (a spec)."""
        for entry in _search_locations(parent):
            if not isinstance(entry, str):
                continue
            finder = cls._path_importer_cache(entry)
            discover = getattr(finder, 'discover', None)
            if discover is not None:
                yield from discover(parent)

    @staticmethod
    def find_distributions(*args, **kwargs):
        """Distributions found on the path (see importlib.metadata)."""
        from importlib.metadata import MetadataPathFinder
        return MetadataPathFinder.find_distributions(*args, **kwargs)


class FileFinder:
    """Path entry finder for one directory.

    The directory listing is cached and re-read when the directory's mtime
    changes.  Loaders are tried in the order of the (loader, suffixes) pairs
    given to the constructor.
    """

    def __init__(self, path, *loader_details):
        self._loaders = [(suffix, loader)
                         for loader, suffixes in loader_details
                         for suffix in suffixes]
        self.path = _os.getcwd() if (not path or path == '.') else _path_abspath(path)
        self._path_mtime = -1
        self._path_cache = set()
        self._relaxed_path_cache = set()

    def invalidate_caches(self):
        """Re-read the directory on the next lookup."""
        self._path_mtime = -1

    def _get_spec(self, loader_class, fullname, path, smsl, target):
        return spec_from_file_location(fullname, path, loader=loader_class(fullname, path),
                                       submodule_search_locations=smsl)

    def _listing(self):
        """(names in the directory, whether to compare lower-cased names)."""
        try:
            mtime = _path_stat(self.path or _os.getcwd()).st_mtime
        except OSError:
            mtime = -1
        if mtime != self._path_mtime:
            self._fill_cache()
            self._path_mtime = mtime
        if _relax_case():
            return self._relaxed_path_cache, True
        return self._path_cache, False

    def find_spec(self, fullname, target=None):
        """Spec for *fullname* in this directory: a package directory with
        an __init__ file, a module file, or a namespace portion."""
        tail = fullname.rpartition('.')[2]
        listing, lowered = self._listing()
        key = tail.lower() if lowered else tail
        namespace_dir = None
        if key in listing:
            directory = _path_join(self.path, tail)
            for suffix, loader_class in self._loaders:
                init_file = _path_join(directory, '__init__' + suffix)
                if _path_isfile(init_file):
                    return self._get_spec(loader_class, fullname, init_file, [directory], target)
            if _path_isdir(directory):
                namespace_dir = directory
        for suffix, loader_class in self._loaders:
            try:
                candidate = _path_join(self.path, tail + suffix)
            except ValueError:
                return None
            _bootstrap._verbose_message('trying {}', candidate, verbosity=2)
            if key + suffix in listing and _path_isfile(candidate):
                return self._get_spec(loader_class, fullname, candidate, None, target)
        if namespace_dir is not None:
            _bootstrap._verbose_message('possible namespace for {}', namespace_dir)
            spec = _bootstrap.ModuleSpec(fullname, None)
            spec.submodule_search_locations = [namespace_dir]
            return spec
        return None

    def discover(self, parent=None):
        """Yield specs for the modules and packages in this directory
        (submodules of the package spec *parent* when given)."""
        if parent is not None and parent.submodule_search_locations is None:
            raise ValueError(f'{parent.name!r} is not a package')
        prefix = parent.name + '.' if parent is not None else ''
        listing, _ = self._listing()
        names = set()
        for entry in sorted(self._path_cache):
            stem = entry
            for suffix, _loader in self._loaders:
                if entry.endswith(suffix) and len(entry) > len(suffix):
                    stem = entry[:-len(suffix)]
                    break
            if not stem.isidentifier() or stem == '__init__' or '.' in stem:
                continue
            names.add(stem)
        for name in sorted(names):
            spec = self.find_spec(prefix + name)
            if spec is not None:
                yield spec

    def _fill_cache(self):
        """Read the directory listing."""
        try:
            contents = _os.listdir(self.path or _os.getcwd())
        except OSError:
            contents = []   # missing, unreadable or not a directory
        self._path_cache = set(contents)
        if sys.platform.startswith(_CASE_INSENSITIVE_PLATFORMS):
            self._relaxed_path_cache = {name.lower() for name in contents}

    @classmethod
    def path_hook(cls, *loader_details):
        """A sys.path_hooks callable making FileFinders for directories."""
        def path_hook_for_FileFinder(path):
            """Path hook for importlib.machinery.FileFinder."""
            if not _path_isdir(path):
                raise ImportError('only directories are supported', path=path)
            return cls(path, *loader_details)
        return path_hook_for_FileFinder

    def __repr__(self):
        return f'FileFinder({self.path!r})'


class WindowsRegistryFinder:
    """Meta path finder for modules registered in the Windows registry
    (deprecated).  The registry does not exist on this platform, so it finds
    nothing."""

    @classmethod
    def find_spec(cls, fullname, path=None, target=None):
        _warn('importlib.machinery.WindowsRegistryFinder is deprecated', DeprecationWarning,
              stacklevel=2)
        return None


# ---------------------------------------------------------------- installation

def _fix_up_module(ns, name, pathname, cpathname=None):
    """Give the namespace of a module executed from a file outside the
    import system (PyImport_ExecCodeModule*) a loader and a spec."""
    loader = ns.get('__loader__')
    spec = ns.get('__spec__')
    if not loader:
        if spec:
            loader = spec.loader
        elif pathname == cpathname:
            loader = SourcelessFileLoader(name, pathname)
        else:
            loader = SourceFileLoader(name, pathname)
    if not spec:
        spec = spec_from_file_location(name, pathname, loader=loader)
        if cpathname:
            spec.cached = _path_abspath(cpathname)
    try:
        ns['__spec__'] = spec
        ns['__loader__'] = loader
        ns['__file__'] = pathname
    except Exception:
        # not a writable mapping: leave it alone
        pass


def _get_supported_file_loaders():
    """(loader, suffixes) pairs of the default file finder, in lookup order."""
    return [
        (ExtensionFileLoader, _imp.extension_suffixes()),
        (SourceFileLoader, SOURCE_SUFFIXES),
        (SourcelessFileLoader, BYTECODE_SUFFIXES),
    ]


def _set_bootstrap_module(_bootstrap_module):
    global _bootstrap
    _bootstrap = _bootstrap_module


def _install(_bootstrap_module):
    """Install the path finder and the default file finder hook."""
    _set_bootstrap_module(_bootstrap_module)
    sys.path_hooks.append(FileFinder.path_hook(*_get_supported_file_loaders()))
    sys.meta_path.append(PathFinder)


EXTENSION_SUFFIXES.extend(_imp.extension_suffixes())
