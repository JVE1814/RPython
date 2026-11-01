"""Standard codec package.

Each codec lives in a module encodings.<name> whose getregentry() returns a
codecs.CodecInfo.  The search function registered here maps an encoding name
to such a module: the name is normalized (see normalize_encoding), looked up
in encodings.aliases, and the module is imported on first use.  Results
(including misses) are cached in _cache.
"""

import codecs
import sys
from . import aliases

_cache = {}
_MAXCACHE = 500
_unknown = '--unknown--'
_import_tail = ['*']
_aliases = aliases.aliases


class CodecRegistryError(LookupError, SystemError):
    pass


def normalize_encoding(encoding):
    """Normalize an encoding name for module lookup.

    Every run of characters other than ASCII letters, digits and '.' is
    replaced by a single underscore; leading and trailing runs are removed.
    The case is preserved.  Non-ASCII letters and digits are dropped
    (deprecated).
    """
    if isinstance(encoding, bytes):
        encoding = str(encoding, "ascii")

    if not encoding.isascii():
        import warnings
        warnings.warn(
            "Support for non-ascii encoding names will be removed in 3.17",
            DeprecationWarning, stacklevel=2)

    out = []
    pending_sep = False
    for c in encoding:
        if c.isalnum() or c == '.':
            if pending_sep and out:
                out.append('_')
            if c.isascii():
                out.append(c)
            pending_sep = False
        else:
            pending_sep = True
    return ''.join(out)


def _remember(encoding, entry):
    if len(_cache) >= _MAXCACHE:
        try:
            del _cache[next(iter(_cache))]
        except (StopIteration, RuntimeError, KeyError):
            pass
    _cache[encoding] = entry


def search_function(encoding):
    entry = _cache.get(encoding, _unknown)
    if entry is not _unknown:
        return entry

    # Candidate module names: the alias target (first with the name as it
    # is, then with dots turned into underscores), then the name itself.
    norm_encoding = normalize_encoding(encoding)
    aliased_encoding = (_aliases.get(norm_encoding) or
                        _aliases.get(norm_encoding.replace('.', '_')))
    if aliased_encoding is not None:
        modnames = [aliased_encoding, norm_encoding]
    else:
        modnames = [norm_encoding]

    mod = None
    for modname in modnames:
        if not modname or '.' in modname:
            continue
        try:
            # Import relative to this package only.
            mod = __import__('encodings.' + modname, fromlist=_import_tail,
                             level=0)
        except ImportError:
            pass
        else:
            break

    try:
        getregentry = mod.getregentry
    except AttributeError:
        # Not a codec module.
        mod = None

    if mod is None:
        _remember(encoding, None)
        return None

    entry = getregentry()
    if not isinstance(entry, codecs.CodecInfo):
        if not 4 <= len(entry) <= 7:
            raise CodecRegistryError('module "%s" (%s) failed to register'
                                     % (mod.__name__, mod.__file__))
        if not callable(entry[0]) or not callable(entry[1]) or \
           (entry[2] is not None and not callable(entry[2])) or \
           (entry[3] is not None and not callable(entry[3])) or \
           (len(entry) > 4 and entry[4] is not None and not callable(entry[4])) or \
           (len(entry) > 5 and entry[5] is not None and not callable(entry[5])):
            raise CodecRegistryError('incompatible codecs in module "%s" (%s)'
                                     % (mod.__name__, mod.__file__))
        if len(entry) < 7 or entry[6] is None:
            entry += (None,) * (6 - len(entry)) + (mod.__name__.split(".", 1)[1],)
        entry = codecs.CodecInfo(*entry)

    _remember(encoding, entry)

    # Register the aliases the module declares itself.
    try:
        codecaliases = mod.getaliases()
    except AttributeError:
        pass
    else:
        for alias in codecaliases:
            if alias not in _aliases:
                _aliases[alias] = modname

    return entry


codecs.register(search_function)

if sys.platform == 'win32':
    def _alias_mbcs(encoding):
        try:
            import _winapi
            ansi_code_page = "cp%s" % _winapi.GetACP()
            if encoding == ansi_code_page:
                import encodings.mbcs
                return encodings.mbcs.getregentry()
        except ImportError:
            pass

    codecs.register(_alias_mbcs)
