"""Cache lines from Python source files.

This is intended to read lines from modules imported -- hence if a filename
is not found, it will look down the module search path for a file by
that name.
"""

__all__ = ["getline", "clearcache", "checkcache", "lazycache"]


# The cache. Maps filenames to either a thunk which will provide source code,
# or a tuple (size, mtime, lines, fullname) once loaded.
cache = {}
_interactive_cache = {}


def clearcache():
    """Clear the cache entirely."""
    cache.clear()


def getline(filename, lineno, module_globals=None):
    """Get a line for a Python source file from the cache.
    Update the cache if it doesn't contain an entry for this file already."""
    lines = getlines(filename, module_globals)
    if 1 <= lineno <= len(lines):
        return lines[lineno - 1]
    return ''


def getlines(filename, module_globals=None):
    """Get the lines for a Python source file from the cache.
    Update the cache if it doesn't contain an entry for this file already."""
    entry = cache.get(filename)
    if entry is not None and len(entry) != 1:
        return entry[2]
    try:
        return updatecache(filename, module_globals)
    except MemoryError:
        clearcache()
        return []


def _getline_from_code(code, lineno):
    lines = _getlines_from_code(code)
    if 1 <= lineno <= len(lines):
        return lines[lineno - 1]
    return ''


def _make_key(code):
    return (code.co_filename, code.co_qualname, code.co_firstlineno)


def _getlines_from_code(code):
    code_id = _make_key(code)
    entry = _interactive_cache.get(code_id)
    if entry is not None and len(entry) != 1:
        return entry[2]
    return []


def _source_unavailable(filename):
    """Return True if the source code is unavailable for such file name."""
    return (
        not filename
        or (filename.startswith('<')
            and filename.endswith('>')
            and not filename.startswith('<frozen '))
    )


def checkcache(filename=None):
    """Discard cache entries that are out of date.
    (This is not checked upon each call!)"""

    if filename is None:
        # get keys atomically
        filenames = list(cache)
    else:
        filenames = [filename]

    for filename in filenames:
        entry = cache.get(filename)
        if entry is None:
            continue
        if len(entry) == 1:
            # lazy cache entry, leave it lazy.
            continue
        size, mtime, lines, fullname = entry
        if mtime is None:
            continue   # no-op for files loaded via a __loader__
        import os
        try:
            # This import can fail if the interpreter is shutting down
            stat = os.stat(fullname)
        except (OSError, ValueError):
            cache.pop(filename, None)
            continue
        if size != stat.st_size or mtime != stat.st_mtime:
            cache.pop(filename, None)


def _decode_source(data):
    """Decode Python source bytes honouring a BOM or a PEP 263 cookie."""
    encoding = 'utf-8'
    if data.startswith(b'\xef\xbb\xbf'):
        data = data[3:]
        encoding = 'utf-8'
    else:
        # look for a coding cookie on the first two lines
        pos = 0
        for _ in range(2):
            nl = data.find(b'\n', pos)
            line = data[pos:] if nl < 0 else data[pos:nl + 1]
            stripped = line.lstrip(b' \t\f')
            if stripped.startswith(b'#'):
                enc = _find_cookie(line)
                if enc is not None:
                    encoding = enc
                    break
            elif stripped.strip():
                break
            if nl < 0:
                break
            pos = nl + 1
    text = data.decode(encoding)
    return text


def _find_cookie(line):
    try:
        s = line.decode('latin-1')
    except Exception:
        return None
    i = s.find('coding')
    while i >= 0:
        j = i + 6
        if j < len(s) and s[j] in ':=':
            j += 1
            while j < len(s) and s[j] in ' \t':
                j += 1
            k = j
            while k < len(s) and (s[k].isalnum() or s[k] in '-_.'):
                k += 1
            if k > j:
                name = s[j:k]
                low = name.lower().replace('_', '-')
                if low == 'utf-8' or low.startswith('utf-8-'):
                    return 'utf-8'
                if low in ('latin-1', 'iso-8859-1', 'iso-latin-1') or \
                        low.startswith(('latin-1-', 'iso-8859-1-', 'iso-latin-1-')):
                    return 'iso-8859-1'
                return name
        i = s.find('coding', i + 1)
    return None


def _read_lines(fullname):
    # Read with os-level calls: linecache is used while printing tracebacks,
    # possibly before (or without) the io machinery being usable.
    import os
    fd = os.open(fullname, os.O_RDONLY | getattr(os, "O_CLOEXEC", 0))
    try:
        chunks = []
        while True:
            chunk = os.read(fd, 1 << 16)
            if not chunk:
                break
            chunks.append(chunk)
    finally:
        os.close(fd)
    data = b"".join(chunks)
    text = _decode_source(data)
    text = text.replace('\r\n', '\n').replace('\r', '\n')
    return _split_nl(text)


def _split_nl(text):
    lines = []
    start = 0
    while True:
        i = text.find('\n', start)
        if i < 0:
            if start < len(text):
                lines.append(text[start:])
            return lines
        lines.append(text[start:i + 1])
        start = i + 1


def _zip_source(fullname):
    """Return the source of *fullname* if it lies inside a zip archive
    that is on the import path, else None."""
    import sys
    import os
    pic = getattr(sys, 'path_importer_cache', None)
    if not isinstance(pic, dict):
        return None
    head = fullname
    rest = []
    while True:
        parent, tail = os.path.split(head)
        if not tail or parent == head:
            return None
        rest.append(tail)
        head = parent
        try:
            if os.path.isfile(head):
                break
        except (OSError, ValueError):
            return None
        if os.path.isdir(head):
            return None
    rest.reverse()
    if not rest or not rest[-1].endswith('.py'):
        return None
    try:
        import zipimport
    except ImportError:
        return None
    inner = rest[:-1]
    modname = rest[-1][:-3]
    try:
        archive_path = os.path.join(head, *inner) if inner else head
        importer = zipimport.zipimporter(archive_path)
        if modname == '__init__':
            if not inner:
                return None
            importer = zipimport.zipimporter(os.path.join(head, *inner[:-1])
                                             if len(inner) > 1 else head)
            modname = inner[-1]
        return importer.get_source(modname)
    except Exception:
        return None


def updatecache(filename, module_globals=None):
    """Update a cache entry and return its list of lines.
    If something's wrong, print a message, discard the cache entry,
    and return an empty list."""

    # These imports are not at top level because linecache is in the critical
    # path of the interpreter startup and importing os and sys take a lot of time
    # and slows down the startup sequence.
    try:
        import os
        import sys
    except ImportError:
        # These import can fail if the interpreter is shutting down
        return []

    entry = cache.pop(filename, None)
    if _source_unavailable(filename):
        return []

    if filename.startswith('<frozen '):
        if module_globals is None or not isinstance(module_globals, dict):
            return []
        fullname = module_globals.get('__file__')
        if not fullname:
            return []
    else:
        fullname = filename
    try:
        stat = os.stat(fullname)
    except OSError:
        basename = filename

        # Realise a lazy loader based lookup if there is one
        # otherwise try to lookup right now.
        lazy_entry = entry if entry is not None and len(entry) == 1 else None
        if lazy_entry is None:
            lazy_entry = _make_lazycache_entry(filename, module_globals, bless=True)
        if lazy_entry is not None:
            try:
                data = lazy_entry[0]()
            except (ImportError, OSError):
                pass
            else:
                if data is None:
                    # No luck, the PEP302 loader cannot find the source
                    # for this module.
                    return []
                cache[filename] = (
                    len(data),
                    None,
                    [line + '\n' for line in data.splitlines()],
                    fullname
                )
                return cache[filename][2]

        data = _zip_source(fullname)
        if data is not None:
            lines = data.splitlines(True)
            if lines and not lines[-1].endswith('\n'):
                lines[-1] += '\n'
            cache[filename] = (len(data), None, lines, fullname)
            return lines

        # Try looking through the module search path, which is only useful
        # when handling a relative filename.
        if os.path.isabs(filename):
            return []

        for dirname in sys.path:
            try:
                fullname = os.path.join(dirname, basename)
            except (TypeError, AttributeError):
                # Not sufficiently string-like to do anything useful with.
                continue
            try:
                stat = os.stat(fullname)
                break
            except (OSError, ValueError):
                pass
        else:
            return []
    except ValueError:  # may be raised by os.stat()
        return []
    try:
        lines = _read_lines(fullname)
    except (OSError, UnicodeDecodeError, SyntaxError, LookupError):
        return []
    if not lines:
        lines = ['\n']
    elif not lines[-1].endswith('\n'):
        lines[-1] += '\n'
    size, mtime = stat.st_size, stat.st_mtime
    cache[filename] = size, mtime, lines, fullname
    return lines


def _bless_my_loader(module_globals):
    missing = object()
    loader = module_globals.get('__loader__', None)
    spec = module_globals.get('__spec__', missing)
    if spec is missing and loader is None:
        # neither is given: there is simply no loader to use
        return None
    spec_loader = getattr(spec, 'loader', missing) if spec is not missing else missing
    if spec is None:
        spec_loader = None
    if spec_loader is missing or spec_loader is None:
        if loader is None:
            if spec_loader is missing:
                raise AttributeError('Module globals is missing a __spec__.loader')
            raise ValueError('Module globals is missing a __spec__.loader')
        import warnings
        warnings.warn('Module globals is missing a __spec__.loader',
                      DeprecationWarning)
        spec_loader = loader
    if loader is not None and loader != spec_loader:
        import warnings
        warnings.warn('Module globals; __loader__ != __spec__.loader',
                      DeprecationWarning)
        return loader
    return spec_loader


def _make_lazycache_entry(filename, module_globals, bless=False):
    if not filename or (filename.startswith('<') and filename.endswith('>')):
        return None
    # Try for a __loader__, if available
    if module_globals and '__name__' in module_globals:
        spec = module_globals.get('__spec__')
        name = getattr(spec, 'name', None) or module_globals['__name__']
        if bless:
            # Validate __loader__ against __spec__.loader (deprecation
            # warnings for inconsistent module globals).
            try:
                loader = _bless_my_loader(module_globals)
            except (AttributeError, ValueError) as exc:
                import warnings
                warnings.warn(str(exc), DeprecationWarning)
                return None
            except ImportError:
                return None
        else:
            loader = getattr(spec, 'loader', None)
            if loader is None:
                loader = module_globals.get('__loader__')
        get_source = getattr(loader, 'get_source', None)
        if name and get_source:
            def get_lines(name=name, *args, **kwargs):
                return get_source(name, *args, **kwargs)
            return (get_lines,)
    return None


def lazycache(filename, module_globals):
    """Seed the cache for filename with module_globals.

    The module loader will be asked for the source only when getlines is
    called, not immediately.

    If there is an entry in the cache already, it is not altered.

    :return: True if a lazy load is registered in the cache,
        otherwise False. To register such a load a module loader with a
        get_source method must be found, the filename must be a cacheable
        filename, and the filename must not be already cached.
    """
    entry = cache.get(filename)
    if entry is not None:
        return len(entry) == 1

    lazy_entry = _make_lazycache_entry(filename, module_globals)
    if lazy_entry is not None:
        cache[filename] = lazy_entry
        return True
    return False


def _register_code(code, string, name):
    entry = (len(string),
             None,
             [line + '\n' for line in string.splitlines()],
             name)
    stack = [code]
    while stack:
        code = stack.pop()
        for const in code.co_consts:
            if isinstance(const, type(code)):
                stack.append(const)
        key = _make_key(code)
        _interactive_cache[key] = entry


def _take_interpreter_sources():
    # Sources the interpreter recorded before this module was first imported
    # (the code of `python -c`), registered now instead of at every start.
    try:
        from _imp import _rpy_linecache_pending
    except ImportError:
        return
    for code, string, name in _rpy_linecache_pending():
        _register_code(code, string, name)


_take_interpreter_sources()
del _take_interpreter_sources
