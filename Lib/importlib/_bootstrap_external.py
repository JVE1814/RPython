"""Path-based import machinery: source/bytecode/extension loaders, the path
finder, file finders, namespace packages and bytecode-cache helpers.

Loaded at start-up as ``_frozen_importlib_external`` (after ``_bootstrap``)
and installed with ``_install(_bootstrap_module)``.  Only built-in modules
may be imported here.
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


class _FallbackStat:
    __slots__ = ('st_mode', 'st_size', 'st_mtime')

    def __init__(self, mode, size, mtime):
        self.st_mode = mode
        self.st_size = size
        self.st_mtime = mtime


class _FallbackOS:
    """Minimal file-system access used until a real ``posix`` module exists."""

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
        try:
            func = type(path).__fspath__
        except AttributeError:
            raise TypeError('expected str, bytes or os.PathLike object, not '
                            + type(path).__name__) from None
        res = func(path)
        if not isinstance(res, (str, bytes)):
            raise TypeError(f'expected {type(path).__name__}.__fspath__() to '
                            f'return str or bytes, not {type(res).__name__}')
        return res

    @staticmethod
    def mkdir(path, mode=0o777):
        raise OSError('directory creation not supported')


if 'posix' in sys.builtin_module_names:
    import posix as _os
else:
    _os = _FallbackOS()

_bootstrap = None

path_sep = '/'
path_separators = '/'
_pathseps_with_colon = {f':{s}' for s in path_separators}

_CASE_INSENSITIVE_PLATFORMS_STR_KEY = 'win',
_CASE_INSENSITIVE_PLATFORMS_BYTES_KEY = 'cygwin', 'darwin', 'ios', 'tvos', 'watchos'
_CASE_INSENSITIVE_PLATFORMS = (_CASE_INSENSITIVE_PLATFORMS_BYTES_KEY
                               + _CASE_INSENSITIVE_PLATFORMS_STR_KEY)


def _warn(message, category, stacklevel=1):
    if _warnings is not None:
        _warnings.warn(message, category, stacklevel + 1)
    else:
        try:
            import warnings
        except ImportError:
            return
        warnings.warn(message, category, stacklevel + 1)


def _make_relax_case():
    if sys.platform.startswith(_CASE_INSENSITIVE_PLATFORMS):
        if sys.platform.startswith(_CASE_INSENSITIVE_PLATFORMS_STR_KEY):
            key = 'PYTHONCASEOK'
        else:
            key = b'PYTHONCASEOK'

        def _relax_case():
            """True if filenames must be checked case-insensitively and ignore
            environment flags are not set."""
            return not sys.flags.ignore_environment and key in _os.environ
    else:
        def _relax_case():
            """True if filenames must be checked case-insensitively."""
            return False
    return _relax_case


_relax_case = _make_relax_case()


def _pack_uint32(x):
    """Convert a 32-bit integer to little-endian."""
    return (int(x) & 0xFFFFFFFF).to_bytes(4, 'little')


def _unpack_uint32(data):
    """Convert 4 bytes in little-endian to an integer."""
    assert len(data) == 4
    return int.from_bytes(data, 'little')


def _unpack_uint16(data):
    """Convert 2 bytes in little-endian to an integer."""
    assert len(data) == 2
    return int.from_bytes(data, 'little')


def _path_join(*path_parts):
    """Join path components with the platform separator."""
    if not path_parts:
        return ''
    if len(path_parts) == 1:
        return path_parts[0]
    return path_sep.join([part.rstrip(path_separators)
                          for part in path_parts if part])


def _path_split(path):
    """Split a path at its last separator: (head, tail)."""
    i = path.rfind(path_sep)
    if i < 0:
        return '', path
    return path[:i], path[i + 1:]


def _path_stat(path):
    """os.stat() of the path (indirection that tests can replace)."""
    return _os.stat(path)


def _path_is_mode_type(path, mode):
    try:
        stat_info = _path_stat(path)
    except OSError:
        return False
    return (stat_info.st_mode & 0o170000) == mode


def _path_isfile(path):
    return _path_is_mode_type(path, 0o100000)


def _path_isdir(path):
    if not path:
        path = _os.getcwd()
    return _path_is_mode_type(path, 0o040000)


def _path_isabs(path):
    return path.startswith(path_separators)


def _path_abspath(path):
    """Make the path absolute without touching the file system."""
    if not _path_isabs(path):
        for sep in path_separators:
            path = path.removeprefix(f".{sep}")
        return _path_join(_os.getcwd(), path)
    return path


def _write_atomic(path, data, mode=0o666):
    """Write data to a temporary file next to path, then rename it into
    place so that readers never see a partial file."""
    path_tmp = f'{path}.{id(path)}'
    fd = _os.open(path_tmp,
                  _os.O_EXCL | _os.O_CREAT | _os.O_WRONLY, mode & 0o666)
    try:
        with _io.FileIO(fd, 'wb') as file:
            bytes_written = file.write(data)
        if bytes_written != len(data):
            raise OSError("Short write when writing pyc file")
        _os.replace(path_tmp, path)
    except OSError:
        try:
            _os.unlink(path_tmp)
        except OSError:
            pass
        raise


_code_type = type(_write_atomic.__code__)

# Bytecode format number of this implementation (our bytecode differs from
# CPython's, so .pyc files are never shared with it).
MAGIC_NUMBER = _imp.pyc_magic_number_token.to_bytes(4, 'little')

_RAW_MAGIC_NUMBER = int.from_bytes(MAGIC_NUMBER, 'little')

_PYCACHE = '__pycache__'
_OPT = 'opt-'

SOURCE_SUFFIXES = ['.py']

BYTECODE_SUFFIXES = ['.pyc']

EXTENSION_SUFFIXES = []


def cache_from_source(path, debug_override=None, *, optimization=None):
    """Return the path of the .pyc file for the source file at *path*."""
    if debug_override is not None:
        _warn('the debug_override parameter is deprecated; use '
              "'optimization' instead", DeprecationWarning)
        if optimization is not None:
            message = 'debug_override or optimization must be set to None'
            raise TypeError(message)
        optimization = '' if debug_override else 1
    path = _os.fspath(path)
    head, tail = _path_split(path)
    base, sep, rest = tail.rpartition('.')
    tag = sys.implementation.cache_tag
    if tag is None:
        raise NotImplementedError('sys.implementation.cache_tag is None')
    almost_filename = ''.join([(base if base else rest), sep, tag])
    if optimization is None:
        if sys.flags.optimize == 0:
            optimization = ''
        else:
            optimization = sys.flags.optimize
    optimization = str(optimization)
    if optimization != '':
        if not optimization.isalnum():
            raise ValueError(f'{optimization!r} is not alphanumeric')
        almost_filename = f'{almost_filename}.{_OPT}{optimization}'
    filename = almost_filename + BYTECODE_SUFFIXES[0]
    if sys.pycache_prefix is not None:
        if not _path_isabs(head):
            head = _path_join(_os.getcwd(), head)
        return _path_join(
            sys.pycache_prefix,
            head.lstrip(path_separators),
            filename,
        )
    return _path_join(head, _PYCACHE, filename)


def source_from_cache(path):
    """Return the source path of the .pyc file at *path* (inverse of
    cache_from_source())."""
    if sys.implementation.cache_tag is None:
        raise NotImplementedError('sys.implementation.cache_tag is None')
    path = _os.fspath(path)
    head, pycache_filename = _path_split(path)
    found_in_pycache_prefix = False
    if sys.pycache_prefix is not None:
        stripped_path = sys.pycache_prefix.rstrip(path_separators)
        if head.startswith(stripped_path + path_sep):
            head = head[len(stripped_path):]
            found_in_pycache_prefix = True
    if not found_in_pycache_prefix:
        head, pycache = _path_split(head)
        if pycache != _PYCACHE:
            raise ValueError(f'{_PYCACHE} not bottom-level directory in '
                             f'{path!r}')
    dot_count = pycache_filename.count('.')
    if dot_count not in {2, 3}:
        raise ValueError(f'expected only 2 or 3 dots in {pycache_filename!r}')
    elif dot_count == 3:
        optimization = pycache_filename.rsplit('.', 2)[-2]
        if not optimization.startswith(_OPT):
            raise ValueError("optimization portion of filename does not start "
                             f"with {_OPT!r}")
        opt_level = optimization[len(_OPT):]
        if not opt_level.isalnum():
            raise ValueError(f"optimization level {optimization!r} is not an "
                             "alphanumeric value")
    base_filename = pycache_filename.partition('.')[0]
    return _path_join(head, base_filename + SOURCE_SUFFIXES[0])


def _get_sourcefile(bytecode_path):
    """Map a .pyc path to its source file if that exists, else return the
    bytecode path itself (PyImport_ExecCodeModuleWithFilenames helper)."""
    if len(bytecode_path) == 0:
        return None
    rest, _, extension = bytecode_path.rpartition('.')
    if not rest or extension.lower()[-3:-1] != 'py':
        return bytecode_path
    try:
        source_path = source_from_cache(bytecode_path)
    except (NotImplementedError, ValueError):
        source_path = bytecode_path[:-1]
    return source_path if _path_isfile(source_path) else bytecode_path


def _get_cached(filename):
    if filename.endswith(tuple(SOURCE_SUFFIXES)):
        try:
            return cache_from_source(filename)
        except NotImplementedError:
            pass
    elif filename.endswith(tuple(BYTECODE_SUFFIXES)):
        return filename
    else:
        return None


def _calc_mode(path):
    """File mode to use for the bytecode cache of *path*."""
    try:
        mode = _path_stat(path).st_mode
    except OSError:
        mode = 0o666
    # the cache must stay writable by its owner
    mode |= 0o200
    return mode


def _check_name(method):
    """Decorator: make sure the loader handles the requested module name
    (a None name means the loader's own name)."""
    def _check_name_wrapper(self, name=None, *args, **kwargs):
        if name is None:
            name = self.name
        elif self.name != name:
            raise ImportError('loader for %s cannot handle %s' %
                                (self.name, name), name=name)
        return method(self, name, *args, **kwargs)

    if _bootstrap is not None:
        _wrap = _bootstrap._wrap
    else:
        def _wrap(new, old):
            for replace in ['__module__', '__name__', '__qualname__', '__doc__']:
                if hasattr(old, replace):
                    setattr(new, replace, getattr(old, replace))
            new.__dict__.update(old.__dict__)

    _wrap(_check_name_wrapper, method)
    return _check_name_wrapper


def _classify_pyc(data, name, exc_details):
    """Check the header of .pyc data and return its flags field."""
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
        message = f'invalid flags {flags!r} in {name!r}'
        raise ImportError(message, **exc_details)
    return flags


def _validate_timestamp_pyc(data, source_mtime, source_size, name,
                            exc_details):
    """Raise ImportError if the timestamp-based .pyc is stale."""
    if _unpack_uint32(data[8:12]) != (int(source_mtime) & 0xFFFFFFFF):
        message = f'bytecode is stale for {name!r}'
        _bootstrap._verbose_message('{}', message)
        raise ImportError(message, **exc_details)
    if (source_size is not None and
        _unpack_uint32(data[12:16]) != (source_size & 0xFFFFFFFF)):
        raise ImportError(f'bytecode is stale for {name!r}', **exc_details)


def _validate_hash_pyc(data, source_hash, name, exc_details):
    """Raise ImportError if the hash-based .pyc does not match the source."""
    if data[8:16] != source_hash:
        raise ImportError(
            f'hash in bytecode doesn\'t match hash of source {name!r}',
            **exc_details,
        )


def _compile_bytecode(data, name=None, bytecode_path=None, source_path=None):
    """Unmarshal a code object from .pyc payload data."""
    if marshal is None:
        raise ImportError('marshal is not available', name=name,
                          path=bytecode_path)
    code = marshal.loads(data)
    if isinstance(code, _code_type):
        _bootstrap._verbose_message('code object from {!r}', bytecode_path)
        if source_path is not None:
            _imp._fix_co_filename(code, source_path)
        return code
    else:
        raise ImportError(f'Non-code object in {bytecode_path!r}',
                          name=name, path=bytecode_path)


def _code_to_timestamp_pyc(code, mtime=0, source_size=0):
    """Serialise a code object into timestamp-based .pyc data."""
    data = bytearray(MAGIC_NUMBER)
    data.extend(_pack_uint32(0))
    data.extend(_pack_uint32(mtime))
    data.extend(_pack_uint32(source_size))
    data.extend(marshal.dumps(code))
    return data


def _code_to_hash_pyc(code, source_hash, checked=True):
    """Serialise a code object into hash-based .pyc data."""
    data = bytearray(MAGIC_NUMBER)
    flags = 0b1 | checked << 1
    data.extend(_pack_uint32(flags))
    assert len(source_hash) == 8
    data.extend(source_hash)
    data.extend(marshal.dumps(code))
    return data


def _universal_newlines(text):
    return text.replace('\r\n', '\n').replace('\r', '\n')


def decode_source(source_bytes):
    """Decode source bytes to str honouring the coding cookie / BOM, with
    universal newline translation."""
    try:
        import tokenize
        readline = _io.BytesIO(source_bytes).readline
        encoding = tokenize.detect_encoding(readline)[0]
    except ImportError:
        encoding = 'utf-8'
        if source_bytes.startswith(b'\xef\xbb\xbf'):
            encoding = 'utf-8-sig'
    return _universal_newlines(source_bytes.decode(encoding))


# Module specifications #######################################################

_POPULATE = object()


def spec_from_file_location(name, location=None, *, loader=None,
                            submodule_search_locations=_POPULATE):
    """Return a module spec for a file-based module."""
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
                loader = loader_class(name, location)
                spec.loader = loader
                break
        else:
            return None

    if submodule_search_locations is _POPULATE:
        if hasattr(loader, 'is_package'):
            try:
                is_package = loader.is_package(name)
            except ImportError:
                pass
            else:
                if is_package:
                    spec.submodule_search_locations = []
    else:
        spec.submodule_search_locations = submodule_search_locations
    if spec.submodule_search_locations == []:
        if location:
            dirname = _path_split(location)[0]
            spec.submodule_search_locations.append(dirname)

    return spec


def _bless_my_loader(module_globals):
    """Return the loader to trust for a module given its globals, warning
    when __loader__ and __spec__.loader disagree or the spec is missing."""
    missing = object()
    loader = module_globals.get('__loader__', None)
    spec = module_globals.get('__spec__', missing)

    if loader is None:
        if spec is missing:
            return None
        elif spec is None:
            raise ValueError('Module globals is missing a __spec__.loader')

    spec_loader = getattr(spec, 'loader', missing)

    if spec_loader in (missing, None):
        if loader is None:
            exc = AttributeError if spec_loader is missing else ValueError
            raise exc('Module globals is missing a __spec__.loader')
        _warn('Module globals is missing a __spec__.loader',
              DeprecationWarning)
        spec_loader = loader

    assert spec_loader is not None
    if loader is not None and loader != spec_loader:
        _warn('Module globals; __loader__ != __spec__.loader',
              DeprecationWarning)
        return loader

    return spec_loader


# Loaders #####################################################################

class _LoaderBasics:

    """Shared behaviour of the source and sourceless file loaders."""

    def is_package(self, fullname):
        """A module is a package if its file is named __init__.<ext>."""
        filename = _path_split(self.get_filename(fullname))[1]
        filename_base = filename.rsplit('.', 1)[0]
        tail_name = fullname.rpartition('.')[2]
        return filename_base == '__init__' and tail_name != '__init__'

    def create_module(self, spec):
        """Use default semantics for module creation."""

    def exec_module(self, module):
        """Execute the module."""
        code = self.get_code(module.__name__)
        if code is None:
            raise ImportError(f'cannot load module {module.__name__!r} when '
                              'get_code() returns None')
        _bootstrap._call_with_frames_removed(exec, code, module.__dict__)

    def load_module(self, fullname):
        """This method is deprecated."""
        return _bootstrap._load_module_shim(self, fullname)


class SourceLoader(_LoaderBasics):

    def path_mtime(self, path):
        """Return the modification time of *path*; raises OSError when the
        loader cannot tell."""
        raise OSError

    def path_stats(self, path):
        """Return a dict with 'mtime' (and optionally 'size') of *path*."""
        return {'mtime': self.path_mtime(path)}

    def _cache_bytecode(self, source_path, cache_path, data):
        """Store bytecode data (overridable to pass extra information)."""
        return self.set_data(cache_path, data)

    def set_data(self, path, data):
        """Write data to path; the default does nothing (no caching)."""

    def get_source(self, fullname):
        """Return the decoded source of the module."""
        path = self.get_filename(fullname)
        try:
            source_bytes = self.get_data(path)
        except OSError as exc:
            raise ImportError('source not available through get_data()',
                              name=fullname) from exc
        return decode_source(source_bytes)

    def source_to_code(self, data, path, fullname=None, *, _optimize=-1):
        """Compile source data (str or bytes) into a code object."""
        if fullname is None:
            return _bootstrap._call_with_frames_removed(
                compile, data, path, 'exec', dont_inherit=True,
                optimize=_optimize)
        try:
            return _bootstrap._call_with_frames_removed(
                compile, data, path, 'exec', dont_inherit=True,
                optimize=_optimize, module=fullname)
        except TypeError as exc:
            if 'module' not in str(exc):
                raise
            return _bootstrap._call_with_frames_removed(
                compile, data, path, 'exec', dont_inherit=True,
                optimize=_optimize)

    def get_code(self, fullname):
        """Return the module's code object, using (and refreshing) the
        bytecode cache when possible."""
        source_path = self.get_filename(fullname)
        source_mtime = None
        source_bytes = None
        source_hash = None
        hash_based = False
        check_source = True
        try:
            bytecode_path = cache_from_source(source_path)
        except NotImplementedError:
            bytecode_path = None
        else:
            try:
                st = self.path_stats(source_path)
            except OSError:
                pass
            else:
                source_mtime = int(st['mtime'])
                try:
                    data = self.get_data(bytecode_path)
                except OSError:
                    pass
                else:
                    exc_details = {
                        'name': fullname,
                        'path': bytecode_path,
                    }
                    try:
                        flags = _classify_pyc(data, fullname, exc_details)
                        bytes_data = data[16:]
                        hash_based = flags & 0b1 != 0
                        if hash_based:
                            check_source = flags & 0b10 != 0
                            if (_imp.check_hash_based_pycs != 'never' and
                                (check_source or
                                 _imp.check_hash_based_pycs == 'always')):
                                source_bytes = self.get_data(source_path)
                                source_hash = _imp.source_hash(
                                    _RAW_MAGIC_NUMBER,
                                    source_bytes,
                                )
                                _validate_hash_pyc(data, source_hash, fullname,
                                                   exc_details)
                        else:
                            _validate_timestamp_pyc(
                                data,
                                source_mtime,
                                st.get('size'),
                                fullname,
                                exc_details,
                            )
                    except (ImportError, EOFError):
                        pass
                    else:
                        _bootstrap._verbose_message('{} matches {}',
                                                    bytecode_path, source_path)
                        try:
                            return _compile_bytecode(
                                bytes_data, name=fullname,
                                bytecode_path=bytecode_path,
                                source_path=source_path)
                        except (ValueError, EOFError, TypeError, ImportError):
                            # unreadable cache: fall back to the source
                            pass
        if source_bytes is None:
            source_bytes = self.get_data(source_path)
        code_object = self.source_to_code(source_bytes, source_path, fullname)
        _bootstrap._verbose_message('code object from {}', source_path)
        if (not sys.dont_write_bytecode and bytecode_path is not None and
                source_mtime is not None and marshal is not None):
            try:
                if hash_based:
                    if source_hash is None:
                        source_hash = _imp.source_hash(_RAW_MAGIC_NUMBER,
                                                       source_bytes)
                    data = _code_to_hash_pyc(code_object, source_hash,
                                             check_source)
                else:
                    data = _code_to_timestamp_pyc(code_object, source_mtime,
                                                  len(source_bytes))
            except (ValueError, TypeError):
                data = None
            if data is not None:
                try:
                    self._cache_bytecode(source_path, bytecode_path, data)
                except NotImplementedError:
                    pass
        return code_object


def _read_file(path):
    if _io is not None:
        with _io.open_code(str(path)) as file:
            return file.read()
    return _imp._rpy_read_file(_os.fspath(path))


class FileLoader:

    """Base class for loaders of a single file (source, bytecode or
    extension module)."""

    def __init__(self, fullname, path):
        self.name = fullname
        self.path = path

    def __eq__(self, other):
        return (self.__class__ == other.__class__ and
                self.__dict__ == other.__dict__)

    def __hash__(self):
        return hash(self.name) ^ hash(self.path)

    @_check_name
    def load_module(self, fullname):
        """Load a module from a file (deprecated; use exec_module())."""
        return super(FileLoader, self).load_module(fullname)

    @_check_name
    def get_filename(self, fullname):
        """Return the path to the source file as found by the finder."""
        return self.path

    def get_data(self, path):
        """Return the raw bytes of the file at *path*."""
        if isinstance(self, (SourceLoader, ExtensionFileLoader)) or _io is None:
            return _read_file(path)
        with _io.FileIO(path, 'r') as file:
            return file.read()

    @_check_name
    def get_resource_reader(self, module):
        from importlib.readers import FileReader
        return FileReader(self)


class SourceFileLoader(FileLoader, SourceLoader):

    """Loader for Python source files."""

    def path_stats(self, path):
        """Return the mtime and size of the file."""
        st = _path_stat(path)
        return {'mtime': st.st_mtime, 'size': st.st_size}

    def _cache_bytecode(self, source_path, bytecode_path, data):
        mode = _calc_mode(source_path)
        return self.set_data(bytecode_path, data, _mode=mode)

    def set_data(self, path, data, *, _mode=0o666):
        """Write bytes to a file, creating missing parent directories."""
        if _io is None or isinstance(_os, _FallbackOS):
            return
        parent, filename = _path_split(path)
        path_parts = []
        while parent and not _path_isdir(parent):
            parent, part = _path_split(parent)
            path_parts.append(part)
        for part in reversed(path_parts):
            parent = _path_join(parent, part)
            try:
                _os.mkdir(parent)
            except FileExistsError:
                continue
            except OSError as exc:
                _bootstrap._verbose_message('could not create {!r}: {!r}',
                                            parent, exc)
                return
        try:
            _write_atomic(path, data, _mode)
            _bootstrap._verbose_message('created {!r}', path)
        except OSError as exc:
            _bootstrap._verbose_message('could not create {!r}: {!r}', path,
                                        exc)


class SourcelessFileLoader(FileLoader, _LoaderBasics):

    """Loader for bytecode-only (.pyc) files."""

    def get_code(self, fullname):
        path = self.get_filename(fullname)
        data = self.get_data(path)
        exc_details = {
            'name': fullname,
            'path': path,
        }
        _classify_pyc(data, fullname, exc_details)
        return _compile_bytecode(
            data[16:],
            name=fullname,
            bytecode_path=path,
        )

    def get_source(self, fullname):
        """There is no source for a bytecode-only module."""
        return None


class ExtensionFileLoader(FileLoader, _LoaderBasics):

    """Loader for extension modules (shared libraries)."""

    def __init__(self, name, path):
        self.name = name
        self.path = path

    def __eq__(self, other):
        return (self.__class__ == other.__class__ and
                self.__dict__ == other.__dict__)

    def __hash__(self):
        return hash(self.name) ^ hash(self.path)

    def create_module(self, spec):
        """Initialise the extension module (PyInit_* / multi-phase)."""
        module = _bootstrap._call_with_frames_removed(
            _imp.create_dynamic, spec)
        _bootstrap._verbose_message('extension module {!r} loaded from {!r}',
                                    spec.name, self.path)
        return module

    def exec_module(self, module):
        """Run the extension's execution slots."""
        _bootstrap._call_with_frames_removed(_imp.exec_dynamic, module)
        _bootstrap._verbose_message('extension module {!r} executed from {!r}',
                                    self.name, self.path)

    def is_package(self, fullname):
        """True if the shared library is named __init__.<suffix>."""
        file_name = _path_split(self.path)[1]
        return any(file_name == '__init__' + suffix
                   for suffix in EXTENSION_SUFFIXES)

    def get_code(self, fullname):
        """Extension modules have no code object."""
        return None

    def get_source(self, fullname):
        """Extension modules have no source code."""
        return None

    @_check_name
    def get_filename(self, fullname):
        """Return the path to the shared library."""
        return self.path


class AppleFrameworkLoader(ExtensionFileLoader):
    """Extension loader for Apple platforms, where binaries live in a
    Frameworks folder and the module file is a ".fwork" marker pointing at
    them. On other platforms it behaves like ExtensionFileLoader."""

    def create_module(self, spec):
        path = spec.origin
        if path.endswith(".fwork"):
            with _io.FileIO(path, "r") as f:
                target = f.read().decode().strip()
            bundle = path.rpartition("/")[0].rpartition("/")[0] if "/" in path else ""
            spec.origin = _path_join(bundle, target) if bundle else target
            self.path = spec.origin
        module = _bootstrap._call_with_frames_removed(_imp.create_dynamic, spec)
        module.__file__ = path
        return module


class _NamespacePath:
    """The __path__ of a namespace package: recomputed whenever the parent
    path (sys.path or the parent's __path__) or the import epoch changes."""

    _epoch = 0

    def __init__(self, name, path, path_finder):
        self._name = name
        self._path = path
        self._last_parent_path = tuple(self._get_parent_path())
        self._last_epoch = self._epoch
        self._path_finder = path_finder

    def _find_parent_path_names(self):
        """(parent module name, attribute holding its path)."""
        parent, dot, me = self._name.rpartition('.')
        if dot == '':
            return 'sys', 'path'
        return parent, '__path__'

    def _get_parent_path(self):
        parent_module_name, path_attr_name = self._find_parent_path_names()
        return getattr(sys.modules[parent_module_name], path_attr_name)

    def _recalculate(self):
        parent_path = tuple(self._get_parent_path())
        if parent_path != self._last_parent_path or self._epoch != self._last_epoch:
            spec = self._path_finder(self._name, parent_path)
            if spec is not None and spec.loader is None:
                if spec.submodule_search_locations:
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
    """Loader for namespace packages (directories without __init__)."""

    def __init__(self, name, path, path_finder):
        self._path = _NamespacePath(name, path, path_finder)

    def is_package(self, fullname):
        return True

    def get_source(self, fullname):
        return ''

    def get_code(self, fullname):
        return compile('', '<string>', 'exec', dont_inherit=True)

    def create_module(self, spec):
        """Use default semantics for module creation."""

    def exec_module(self, module):
        pass

    def load_module(self, fullname):
        """Load a namespace module (deprecated; use exec_module())."""
        _bootstrap._verbose_message('namespace module loaded with path {!r}',
                                    self._path)
        return _bootstrap._load_module_shim(self, fullname)

    def get_resource_reader(self, module):
        from importlib.readers import NamespaceReader
        return NamespaceReader(self._path)


_NamespaceLoader = NamespaceLoader


# Finders #####################################################################

class PathFinder:

    """Meta path finder searching sys.path (or a package's __path__) through
    the path entry finders made by sys.path_hooks."""

    @staticmethod
    def invalidate_caches():
        """Invalidate the caches of all cached path entry finders and drop
        entries for relative paths or missing finders."""
        for name, finder in list(sys.path_importer_cache.items()):
            if finder is None or not _path_isabs(name):
                del sys.path_importer_cache[name]
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
        """Ask each path hook for a finder for *path*."""
        if sys.path_hooks is not None and not sys.path_hooks:
            _warn('sys.path_hooks is empty', ImportWarning)
        for hook in sys.path_hooks:
            try:
                return hook(path)
            except ImportError:
                continue
        else:
            return None

    @classmethod
    def _path_importer_cache(cls, path):
        """Get the finder for *path* from sys.path_importer_cache, creating
        (and caching) it through the path hooks if needed."""
        if path == '':
            try:
                path = _os.getcwd()
            except (FileNotFoundError, PermissionError):
                return None
        try:
            finder = sys.path_importer_cache[path]
        except KeyError:
            finder = cls._path_hooks(path)
            sys.path_importer_cache[path] = finder
        return finder

    @classmethod
    def _get_spec(cls, fullname, path, target=None):
        """Find the spec for *fullname* along *path*; collects namespace
        portions when only namespace directories are found."""
        namespace_path = []
        for entry in path:
            if not isinstance(entry, str):
                continue
            finder = cls._path_importer_cache(entry)
            if finder is not None:
                spec = finder.find_spec(fullname, target)
                if spec is None:
                    continue
                if spec.loader is not None:
                    return spec
                portions = spec.submodule_search_locations
                if portions is None:
                    raise ImportError('spec missing loader')
                namespace_path.extend(portions)
        else:
            spec = _bootstrap.ModuleSpec(fullname, None)
            spec.submodule_search_locations = namespace_path
            return spec

    @classmethod
    def find_spec(cls, fullname, path=None, target=None):
        """Find the spec for *fullname* on sys.path or *path*."""
        if path is None:
            path = sys.path
        spec = cls._get_spec(fullname, path, target)
        if spec is None:
            return None
        elif spec.loader is None:
            namespace_path = spec.submodule_search_locations
            if namespace_path:
                spec.origin = None
                spec.submodule_search_locations = _NamespacePath(
                    fullname, namespace_path, cls._get_spec)
                return spec
            else:
                return None
        else:
            return spec

    @staticmethod
    def find_distributions(*args, **kwargs):
        """Find distributions (see importlib.metadata)."""
        from importlib.metadata import MetadataPathFinder
        return MetadataPathFinder.find_distributions(*args, **kwargs)


class FileFinder:

    """Path entry finder for one directory, caching its listing."""

    def __init__(self, path, *loader_details):
        """Create a finder for *path*; loader_details are (loader, suffixes)
        pairs tried in order."""
        loaders = []
        for loader, suffixes in loader_details:
            loaders.extend((suffix, loader) for suffix in suffixes)
        self._loaders = loaders
        if not path or path == '.':
            self.path = _os.getcwd()
        else:
            self.path = _path_abspath(path)
        self._path_mtime = -1
        self._path_cache = set()
        self._relaxed_path_cache = set()

    def invalidate_caches(self):
        """Force a re-read of the directory listing."""
        self._path_mtime = -1

    def _get_spec(self, loader_class, fullname, path, smsl, target):
        loader = loader_class(fullname, path)
        return spec_from_file_location(fullname, path, loader=loader,
                                       submodule_search_locations=smsl)

    def find_spec(self, fullname, target=None):
        """Return a spec for *fullname* in this directory, a namespace spec
        for a plain directory, or None."""
        is_namespace = False
        tail_module = fullname.rpartition('.')[2]
        try:
            mtime = _path_stat(self.path or _os.getcwd()).st_mtime
        except OSError:
            mtime = -1
        if mtime != self._path_mtime:
            self._fill_cache()
            self._path_mtime = mtime
        if _relax_case():
            cache = self._relaxed_path_cache
            cache_module = tail_module.lower()
        else:
            cache = self._path_cache
            cache_module = tail_module
        if cache_module in cache:
            base_path = _path_join(self.path, tail_module)
            for suffix, loader_class in self._loaders:
                init_filename = '__init__' + suffix
                full_path = _path_join(base_path, init_filename)
                if _path_isfile(full_path):
                    return self._get_spec(loader_class, fullname, full_path, [base_path], target)
            else:
                is_namespace = _path_isdir(base_path)
        for suffix, loader_class in self._loaders:
            try:
                full_path = _path_join(self.path, tail_module + suffix)
            except ValueError:
                return None
            _bootstrap._verbose_message('trying {}', full_path, verbosity=2)
            if cache_module + suffix in cache:
                if _path_isfile(full_path):
                    return self._get_spec(loader_class, fullname, full_path,
                                          None, target)
        if is_namespace:
            _bootstrap._verbose_message('possible namespace for {}', base_path)
            spec = _bootstrap.ModuleSpec(fullname, None)
            spec.submodule_search_locations = [base_path]
            return spec
        return None

    def _fill_cache(self):
        """Read the directory listing into the caches."""
        path = self.path
        try:
            contents = _os.listdir(path or _os.getcwd())
        except (FileNotFoundError, PermissionError, NotADirectoryError):
            contents = []
        except OSError:
            contents = []
        self._path_cache = set(contents)
        if sys.platform.startswith(_CASE_INSENSITIVE_PLATFORMS):
            self._relaxed_path_cache = {fn.lower() for fn in contents}

    @classmethod
    def path_hook(cls, *loader_details):
        """Return a path hook creating FileFinder instances for
        directories."""
        def path_hook_for_FileFinder(path):
            """Path hook for importlib.machinery.FileFinder."""
            if not _path_isdir(path):
                raise ImportError('only directories are supported', path=path)
            return cls(path, *loader_details)

        return path_hook_for_FileFinder

    def __repr__(self):
        return f'FileFinder({self.path!r})'


# Import setup ###############################################################

def _fix_up_module(ns, name, pathname, cpathname=None):
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
        ns['__cached__'] = cpathname
    except Exception:
        pass


def _get_supported_file_loaders():
    """List of (loader, suffixes) for file-based modules."""
    extensions = ExtensionFileLoader, _imp.extension_suffixes()
    source = SourceFileLoader, SOURCE_SUFFIXES
    bytecode = SourcelessFileLoader, BYTECODE_SUFFIXES
    return [extensions, source, bytecode]


def _set_bootstrap_module(_bootstrap_module):
    global _bootstrap
    _bootstrap = _bootstrap_module


def _install(_bootstrap_module):
    """Install the path-based import components."""
    _set_bootstrap_module(_bootstrap_module)
    supported_loaders = _get_supported_file_loaders()
    sys.path_hooks.extend([FileFinder.path_hook(*supported_loaders)])
    sys.meta_path.append(PathFinder)


EXTENSION_SUFFIXES.extend(_imp.extension_suffixes())
