"""Random access to the lines of source files, with caching.

Used by traceback, inspect, pdb and warnings to show source lines.  A file
is read once and kept in ``cache``; names that are not found as given are
also looked up through the module's loader, inside zip archives and along
sys.path.  Line numbers start at 1; missing lines read as ''.
"""

__all__ = ["getline", "clearcache", "checkcache", "lazycache"]

# filename -> (size, mtime, lines, fullname) once read; mtime is None for
# sources that did not come from a plain file.  A 1-tuple (get_lines,) is a
# "lazy" entry: get_lines() asks a module loader for the source.
cache = {}

# Sources of code that has no file (interactive input, ``python -c``), keyed
# by _make_key(code); values have the same layout as cache entries.
_interactive_cache = {}


def clearcache():
    """Forget all cached files."""
    cache.clear()


def getline(filename, lineno, module_globals=None):
    """Return line lineno (counting from 1) of filename, or '' if there is
    no such line."""
    lines = getlines(filename, module_globals)
    if 1 <= lineno <= len(lines):
        return lines[lineno - 1]
    return ''


def getlines(filename, module_globals=None):
    """Return the lines of filename (each ending in a newline), reading the
    file into the cache if needed; [] when the source cannot be found."""
    entry = cache.get(filename)
    if entry is not None and len(entry) > 1:
        return entry[2]
    try:
        return updatecache(filename, module_globals)
    except MemoryError:
        clearcache()
        return []


def checkcache(filename=None):
    """Drop cache entries (all, or the one for filename) whose file has
    changed or disappeared since it was read."""
    names = list(cache) if filename is None else [filename]
    for name in names:
        entry = cache.get(name)
        if entry is None or len(entry) == 1:
            continue                    # unknown, or lazy and not read yet
        size, mtime, _, fullname = entry
        if mtime is None:
            continue                    # not from a plain file: keep it
        try:
            import os
            st = os.stat(fullname)
        except (ImportError, OSError, ValueError):
            cache.pop(name, None)
            continue
        if st.st_size != size or st.st_mtime != mtime:
            cache.pop(name, None)


def _uncacheable(filename):
    # '' and pseudo names like '<string>' or '<stdin>' have no source
    # file, but '<frozen ...>' modules can be found through __file__
    return not filename or (filename[0] == '<' and filename[-1] == '>'
                            and not filename.startswith('<frozen '))


def updatecache(filename, module_globals=None):
    """(Re)read filename into the cache and return its lines ([] if the
    source cannot be found)."""
    try:
        import os
        import sys
    except ImportError:                 # interpreter shutdown
        return []

    old = cache.pop(filename, None)
    if _uncacheable(filename):
        return []
    if filename.startswith('<frozen '):
        if not isinstance(module_globals, dict):
            return []
        fullname = module_globals.get('__file__')
        if not fullname:
            return []
    else:
        fullname = filename

    try:
        st = os.stat(fullname)
    except ValueError:                  # NUL characters, unencodable names
        return []
    except OSError:
        st = None

    if st is None:
        # not a file under this name: try a loader, a zip archive, sys.path
        lazy = old if old is not None and len(old) == 1 else None
        if lazy is None:
            lazy = _lazy_entry(filename, module_globals, validate=True)
        if lazy is not None:
            try:
                source = lazy[0]()
            except (ImportError, OSError):
                source = _NOT_FOUND     # fall through to the other lookups
            if source is None:
                return []
            if source is not _NOT_FOUND:
                lines = [line + '\n' for line in source.splitlines()]
                cache[filename] = (len(source), None, lines, fullname)
                return lines

        source = _zip_member_source(fullname)
        if source is not None:
            lines = _as_lines(source)
            cache[filename] = (len(source), None, lines, fullname)
            return lines

        if os.path.isabs(filename):
            return []
        for directory in sys.path:
            try:
                candidate = os.path.join(directory, filename)
            except (TypeError, AttributeError):
                continue                # not a usable path entry
            try:
                st = os.stat(candidate)
            except (OSError, ValueError):
                continue
            fullname = candidate
            break
        else:
            return []

    try:
        lines = _as_lines(_decode(_read_file(fullname)))
    except (OSError, UnicodeDecodeError, SyntaxError, LookupError):
        return []
    cache[filename] = (st.st_size, st.st_mtime, lines, fullname)
    return lines


def lazycache(filename, module_globals):
    """Register module_globals' loader as the source of filename, to be
    asked only when the lines are needed.

    Returns True if filename now has a lazy entry; False if it is already
    cached (lazily or not), cannot be cached, or no loader with get_source()
    is available.
    """
    entry = cache.get(filename)
    if entry is not None:
        return len(entry) == 1
    lazy = _lazy_entry(filename, module_globals)
    if lazy is None:
        return False
    cache[filename] = lazy
    return True


_NOT_FOUND = object()


def _lazy_entry(filename, module_globals, validate=False):
    """A lazy cache entry fetching filename's source from the loader named
    by module_globals, or None."""
    if not filename or (filename[0] == '<' and filename[-1] == '>'):
        return None
    if not module_globals or '__name__' not in module_globals:
        return None
    spec = module_globals.get('__spec__')
    name = getattr(spec, 'name', None) or module_globals['__name__']
    if validate:
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
    if not name or not get_source:
        return None

    def get_lines(name=name, *args, **kwargs):
        return get_source(name, *args, **kwargs)
    return (get_lines,)


def _bless_my_loader(module_globals):
    """The loader of the module with these globals (see importlib)."""
    from importlib._bootstrap_external import _bless_my_loader as choose
    return choose(module_globals)


# -- reading source text ------------------------------------------------------

def _read_file(path):
    # plain os calls: this runs while tracebacks are printed, possibly when
    # the io machinery is not usable
    import os
    fd = os.open(path, os.O_RDONLY | getattr(os, 'O_CLOEXEC', 0))
    try:
        parts = []
        while chunk := os.read(fd, 1 << 16):
            parts.append(chunk)
    finally:
        os.close(fd)
    return b''.join(parts)


def _decode(data):
    """Decode Python source: UTF-8 BOM, else a coding declaration in one of
    the first two lines, else UTF-8."""
    if data.startswith(b'\xef\xbb\xbf'):
        return data[3:].decode('utf-8')
    encoding = 'utf-8'
    for line in data.split(b'\n', 2)[:2]:
        text = line.lstrip(b' \t\f')
        if not text.startswith(b'#'):
            if text.strip():
                break                   # code before any declaration
            continue
        declared = _declared_encoding(line.decode('latin-1'))
        if declared:
            encoding = declared
            break
    return data.decode(encoding)


def _declared_encoding(comment):
    # PEP 263: "coding" followed by ':' or '=' and the encoding name
    pos = comment.find('coding')
    while pos >= 0:
        rest = comment[pos + 6:]
        if rest[:1] in (':', '='):
            rest = rest[1:].lstrip(' \t')
            end = 0
            while end < len(rest) and (rest[end].isalnum() or rest[end] in '-_.'):
                end += 1
            if end:
                name = rest[:end]
                canonical = name.lower().replace('_', '-')
                if canonical == 'utf-8' or canonical.startswith('utf-8-'):
                    return 'utf-8'
                if canonical in ('latin-1', 'iso-8859-1', 'iso-latin-1') or \
                        canonical.startswith(('latin-1-', 'iso-8859-1-',
                                              'iso-latin-1-')):
                    return 'iso-8859-1'
                return name
        pos = comment.find('coding', pos + 1)
    return None


def _as_lines(text):
    """Split text into lines ending in '\\n' (universal newlines); an empty
    text gives one empty line."""
    text = text.replace('\r\n', '\n').replace('\r', '\n')
    if not text:
        return ['\n']
    # only \n separates lines (not the other characters str.splitlines
    # treats as line boundaries, such as form feeds)
    lines = [line + '\n' for line in text.split('\n')]
    if text.endswith('\n'):
        del lines[-1]
    return lines


def _zip_member_source(fullname):
    """The source of a file stored in a zip archive (fullname being
    archive path + member path), or None."""
    import os
    import sys
    if not isinstance(getattr(sys, 'path_importer_cache', None), dict):
        return None                     # interpreter shutdown
    head = fullname
    while True:
        head, tail = os.path.split(head)
        if not head or not tail:
            return None
        try:
            if os.path.isfile(head):
                break                   # head is the archive
            if os.path.isdir(head):
                return None
        except (OSError, ValueError):
            return None
    try:
        import zipimport
        data = zipimport.zipimporter(head).get_data(fullname)
        return _decode(data)
    except Exception:
        return None


# -- code without a source file ---------------------------------------------

def _make_key(code):
    return (code.co_filename, code.co_qualname, code.co_firstlineno)


def _getlines_from_code(code):
    entry = _interactive_cache.get(_make_key(code))
    if entry is not None and len(entry) > 1:
        return entry[2]
    return []


def _getline_from_code(code, lineno):
    lines = _getlines_from_code(code)
    if 1 <= lineno <= len(lines):
        return lines[lineno - 1]
    return ''


def _register_code(code, string, name):
    """Remember string as the source of code and of the code objects nested
    in it."""
    entry = (len(string), None,
             [line + '\n' for line in string.splitlines()], name)
    pending = [code]
    while pending:
        current = pending.pop()
        _interactive_cache[_make_key(current)] = entry
        pending.extend(const for const in current.co_consts
                       if isinstance(const, type(current)))


def _adopt_startup_sources():
    # sources the interpreter recorded before linecache was imported (the
    # code of ``python -c``)
    try:
        from _imp import _rpy_linecache_pending
    except ImportError:
        return
    for code, string, name in _rpy_linecache_pending():
        _register_code(code, string, name)


_adopt_startup_sources()
del _adopt_startup_sources
