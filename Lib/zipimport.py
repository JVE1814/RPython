"""Import Python modules and packages from ZIP archives.

zipimporter instances are path entry finders/loaders for paths that point
into a ZIP file, e.g. "/tmp/lib.zip" or "/tmp/lib.zip/sub/dir".  The
interpreter installs zipimporter as the first entry of sys.path_hooks.
Only modules stored as .py or .pyc members can be imported; members may be
stored, deflated or Zstandard compressed.
"""

import _frozen_importlib_external as _bootstrap_external
import _frozen_importlib as _bootstrap
import _imp
import _io
import marshal
import sys
import time
import _warnings

__all__ = ['ZipImportError', 'zipimporter']

path_sep = _bootstrap_external.path_sep
alt_path_sep = _bootstrap_external.path_separators[1:]


class ZipImportError(ImportError):
    """Raised for problems reading a ZIP archive or importing from it."""


# archive path -> {member path (with path_sep): table-of-contents entry}
# A toc entry is (full path, compression, compressed size, file size,
#                 local header offset, DOS time, DOS date, CRC-32).
_zip_directory_cache = {}

_module_type = type(sys)

_EOCD_SIG = b'PK\x05\x06'
_EOCD_SIZE = 22
_EOCD64_SIG = b'PK\x06\x06'
_EOCD64_SIZE = 56
_EOCD64_LOCATOR_SIG = b'PK\x06\x07'
_EOCD64_LOCATOR_SIZE = 20
_CENTRAL_SIG = b'PK\x01\x02'
_LOCAL_SIG = b'PK\x03\x04'
_MAX_COMMENT = 0xFFFF

_STORED = 0
_DEFLATED = 8
_ZSTANDARD = 93

# (suffix, is bytecode, is package), in lookup order.
_SEARCH_ORDER = (
    (path_sep + '__init__.pyc', True, True),
    (path_sep + '__init__.py', False, True),
    ('.pyc', True, False),
    ('.py', False, False),
)


class zipimporter(_bootstrap_external._LoaderBasics):
    """Finder and loader for modules inside a ZIP archive.

    zipimporter(archivepath) where archivepath is the archive file name,
    optionally followed by a path inside it ("lib.zip/pkg"); the part
    inside the archive becomes `prefix` (with a trailing separator) and the
    archive file name is `archive`.  ZipImportError is raised if the path
    does not lead to a readable ZIP file.
    """

    def __init__(self, path):
        if not isinstance(path, str):
            raise TypeError(f"expected str, not {type(path)!r}")
        if not path:
            raise ZipImportError('archive path is empty', path=path)
        if alt_path_sep:
            path = path.replace(alt_path_sep, path_sep)

        inner = []
        archive = path
        while True:
            try:
                st = _bootstrap_external._path_stat(archive)
            except (OSError, ValueError):
                head, tail = _bootstrap_external._path_split(archive)
                if head == archive:
                    raise ZipImportError('not a Zip file', path=path)
                archive = head
                inner.append(tail)
                continue
            if (st.st_mode & 0o170000) != 0o100000:  # not a regular file
                raise ZipImportError('not a Zip file', path=path)
            break

        if archive not in _zip_directory_cache:
            _zip_directory_cache[archive] = _read_directory(archive)
        self.archive = archive
        prefix = _bootstrap_external._path_join(*reversed(inner)) if inner else ''
        if prefix:
            prefix += path_sep
        self.prefix = prefix

    def find_spec(self, fullname, target=None):
        """Return a ModuleSpec for *fullname* (or None).  A directory in the
        archive without __init__ yields a namespace package portion."""
        is_pkg = _module_kind(self, fullname)
        if is_pkg is not None:
            return _bootstrap.spec_from_loader(fullname, self, is_package=is_pkg)
        relpath = _module_relpath(self, fullname)
        if _is_directory(self, relpath):
            location = f'{self.archive}{path_sep}{relpath}'
            spec = _bootstrap.ModuleSpec(name=fullname, loader=None,
                                         is_package=True)
            spec.submodule_search_locations.append(location)
            return spec
        return None

    def get_code(self, fullname):
        """Return the code object of module *fullname*."""
        code, _, _ = _load_code(self, fullname)
        return code

    def get_data(self, pathname):
        """Return the bytes of the archive member *pathname* (absolute, or
        relative to the archive)."""
        if alt_path_sep:
            pathname = pathname.replace(alt_path_sep, path_sep)
        key = pathname
        if pathname.startswith(self.archive + path_sep):
            key = pathname[len(self.archive + path_sep):]
        try:
            entry = self._get_files()[key]
        except KeyError:
            raise OSError(0, '', key)
        if entry is None:
            return b''
        return _member_data(self.archive, entry)

    def get_filename(self, fullname):
        """Return the __file__ value the module would get (without
        compiling anything)."""
        relpath = _module_relpath(self, fullname)
        files = self._get_files()
        for suffix, is_bytecode, _ in _SEARCH_ORDER:
            candidate = relpath + suffix
            entry = files.get(candidate)
            if entry is None:
                continue
            if is_bytecode:
                # A stale or invalid .pyc is skipped, as get_code() does.
                try:
                    data = _member_data(self.archive, entry)
                    if _check_pyc(self, candidate, fullname, data) is None:
                        continue
                except ImportError:
                    continue
            return entry[0]
        raise ZipImportError(f"can't find module {fullname!r}", name=fullname)

    def get_source(self, fullname):
        """Return the source of *fullname* as str, or None when only
        bytecode is stored."""
        is_pkg = _module_kind(self, fullname)
        if is_pkg is None:
            raise ZipImportError(f"can't find module {fullname!r}",
                                 name=fullname)
        relpath = _module_relpath(self, fullname)
        if is_pkg:
            path = _bootstrap_external._path_join(relpath, '__init__.py')
        else:
            path = f'{relpath}.py'
        entry = self._get_files().get(path)
        if entry is None:
            return None
        return _bootstrap_external.decode_source(_member_data(self.archive, entry))

    def is_package(self, fullname):
        """True if *fullname* is a package (has an __init__ module)."""
        is_pkg = _module_kind(self, fullname)
        if is_pkg is None:
            raise ZipImportError(f"can't find module {fullname!r}",
                                 name=fullname)
        return is_pkg

    def load_module(self, fullname):
        """Load *fullname* (deprecated; use exec_module())."""
        _warnings.warn("zipimport.zipimporter.load_module() is deprecated and "
                       "slated for removal in Python 3.15; use exec_module() "
                       "instead", DeprecationWarning)
        code, is_pkg, modpath = _load_code(self, fullname)
        mod = sys.modules.get(fullname)
        if mod is None or not isinstance(mod, _module_type):
            mod = _module_type(fullname)
            sys.modules[fullname] = mod
        mod.__loader__ = self
        try:
            if is_pkg:
                relpath = _module_relpath(self, fullname)
                mod.__path__ = [_bootstrap_external._path_join(self.archive, relpath)]
            if not hasattr(mod, '__builtins__'):
                mod.__builtins__ = __builtins__
            _bootstrap_external._fix_up_module(mod.__dict__, fullname, modpath)
            exec(code, mod.__dict__)
        except:
            del sys.modules[fullname]
            raise
        try:
            mod = sys.modules[fullname]
        except KeyError:
            raise ImportError(f'Loaded module {fullname!r} not found in sys.modules')
        _bootstrap._verbose_message('import {} # loaded from Zip {}', fullname, modpath)
        return mod

    def get_resource_reader(self, fullname):
        """Resource reader for package *fullname*."""
        from importlib.readers import ZipReader
        return ZipReader(self, fullname)

    def _get_files(self):
        """The archive's table of contents (re-read if it was dropped)."""
        try:
            files = _zip_directory_cache[self.archive]
        except KeyError:
            try:
                files = _zip_directory_cache[self.archive] = _read_directory(self.archive)
            except ZipImportError:
                files = {}
        return files

    def invalidate_caches(self):
        """Re-read the archive's table of contents."""
        _zip_directory_cache.pop(self.archive, None)
        try:
            _zip_directory_cache[self.archive] = _read_directory(self.archive)
        except ZipImportError:
            pass

    def __repr__(self):
        return f'<zipimporter object "{self.archive}{path_sep}{self.prefix}">'


# ---------------------------------------------------------------- lookup

def _module_relpath(importer, fullname):
    return importer.prefix + fullname.rpartition('.')[2]


def _is_directory(importer, path):
    # Directory entries end with a separator (explicit or implicit ones).
    return (path + path_sep) in importer._get_files()


def _module_kind(importer, fullname):
    """True for a package, False for a module, None if not found."""
    relpath = _module_relpath(importer, fullname)
    files = importer._get_files()
    for suffix, _, is_pkg in _SEARCH_ORDER:
        if relpath + suffix in files:
            return is_pkg
    return None


# ---------------------------------------------------------------- reading

def _read_directory(archive):
    """Parse the central directory of *archive* into a toc dict."""
    try:
        fp = _io.open_code(archive)
    except OSError:
        raise ZipImportError(f"can't open Zip file: {archive!r}", path=archive)
    with fp:
        try:
            fp.seek(0, 2)
            file_size = fp.tell()
        except OSError:
            raise ZipImportError(f"can't read Zip file: {archive!r}", path=archive)
        cd_size, cd_offset, count, eocd_pos = _find_end_record(fp, file_size, archive)
        if eocd_pos < cd_size + cd_offset:
            raise ZipImportError(f'bad central directory size or offset: {archive!r}',
                                 path=archive)
        # Data may precede the archive (e.g. a self-extracting stub).
        shift = eocd_pos - cd_size - cd_offset
        try:
            fp.seek(cd_offset + shift)
            directory = fp.read(cd_size)
        except OSError:
            raise ZipImportError(f"can't read Zip file: {archive!r}", path=archive)
        if len(directory) != cd_size:
            raise ZipImportError(f"can't read Zip file: {archive!r}", path=archive)

    files = {}
    pos = 0
    entries = 0
    while pos < len(directory):
        record = directory[pos:pos + 46]
        if len(record) < 46:
            raise EOFError('EOF read where not expected')
        if record[:4] != _CENTRAL_SIG:
            break
        flags = _u16(record, 8)
        compress = _u16(record, 10)
        dostime = _u16(record, 12)
        dosdate = _u16(record, 14)
        crc = _u32(record, 16)
        data_size = _u32(record, 20)
        file_size = _u32(record, 24)
        name_size = _u16(record, 28)
        extra_size = _u16(record, 30)
        comment_size = _u16(record, 32)
        header_offset = _u32(record, 42)
        pos += 46
        raw_name = directory[pos:pos + name_size]
        extra = directory[pos + name_size:pos + name_size + extra_size]
        pos += name_size + extra_size + comment_size
        if len(raw_name) != name_size:
            raise ZipImportError(f"bad central directory in {archive!r}", path=archive)

        if 0xFFFFFFFF in (data_size, file_size, header_offset):
            file_size, data_size, header_offset = _apply_zip64_extra(
                extra, file_size, data_size, header_offset, archive)

        if flags & 0x800:
            try:
                name = raw_name.decode('utf-8')
            except UnicodeDecodeError as e:
                raise ZipImportError(f"bad file name in {archive!r}",
                                     path=archive) from e
        else:
            name = _decode_cp437(raw_name)
        name = name.replace('/', path_sep)
        full = _bootstrap_external._path_join(archive, name)
        files[name] = (full, compress, data_size, file_size,
                       header_offset + shift, dostime, dosdate, crc)
        entries += 1
    _add_implicit_directories(files)
    _bootstrap._verbose_message('zipimport: found {} names in {!r}', entries, archive)
    return files


def _find_end_record(fp, file_size, archive):
    """Locate the end of central directory record (zip64 aware).  Returns
    (directory size, directory offset, entry count, record position)."""
    def fail(msg):
        raise ZipImportError(f'{msg}: {archive!r}', path=archive)

    try:
        start = max(0, file_size - _EOCD_SIZE - _MAX_COMMENT)
        fp.seek(start)
        tail = fp.read()
    except OSError:
        fail("can't read Zip file")
    idx = tail.rfind(_EOCD_SIG)
    while idx >= 0 and len(tail) - idx < _EOCD_SIZE:
        idx = tail.rfind(_EOCD_SIG, 0, idx)
    if idx < 0:
        fail('not a Zip file')
    record = tail[idx:idx + _EOCD_SIZE]
    eocd_pos = start + idx
    count = _u16(record, 10)
    cd_size = _u32(record, 12)
    cd_offset = _u32(record, 16)
    if 0xFFFF == count or 0xFFFFFFFF in (cd_size, cd_offset):
        loc_pos = eocd_pos - _EOCD64_LOCATOR_SIZE
        try:
            fp.seek(loc_pos)
            locator = fp.read(_EOCD64_LOCATOR_SIZE)
        except OSError:
            fail("can't read Zip file")
        if len(locator) == _EOCD64_LOCATOR_SIZE and locator[:4] == _EOCD64_LOCATOR_SIG:
            rec64_pos = loc_pos - _EOCD64_SIZE
            try:
                fp.seek(rec64_pos)
                rec64 = fp.read(_EOCD64_SIZE)
            except OSError:
                fail("can't read Zip file")
            if len(rec64) != _EOCD64_SIZE or rec64[:4] != _EOCD64_SIG:
                fail('corrupt Zip64 file')
            count = _u64(rec64, 32)
            cd_size = _u64(rec64, 40)
            cd_offset = _u64(rec64, 48)
            eocd_pos = rec64_pos
    return cd_size, cd_offset, count, eocd_pos


def _apply_zip64_extra(extra, file_size, data_size, header_offset, archive):
    pos = 0
    while pos + 4 <= len(extra):
        tag = _u16(extra, pos)
        size = _u16(extra, pos + 2)
        body = extra[pos + 4:pos + 4 + size]
        pos += 4 + size
        if tag != 1:
            continue
        k = 0
        values = []
        for current in (file_size, data_size, header_offset):
            if current == 0xFFFFFFFF:
                if k + 8 > len(body):
                    raise ZipImportError(f'bad Zip64 extra data in {archive!r}',
                                         path=archive)
                values.append(_u64(body, k))
                k += 8
            else:
                values.append(current)
        return tuple(values)
    return file_size, data_size, header_offset


def _add_implicit_directories(files):
    """Register every parent directory of each member (with a trailing
    separator) so namespace packages and directory reads work even when
    the archive has no explicit directory entries."""
    for name in list(files):
        parts = name.rstrip(path_sep).split(path_sep)
        for i in range(1, len(parts)):
            d = path_sep.join(parts[:i]) + path_sep
            if d not in files:
                files[d] = None


# Upper half of code page 437 (the legacy ZIP name encoding).  Decoded here
# without the codec machinery: importing an encoding module could itself
# require this importer.
_CP437_HIGH = (
    '\xc7\xfc\xe9\xe2\xe4\xe0\xe5\xe7\xea\xeb\xe8\xef\xee\xec\xc4\xc5'
    '\xc9\xe6\xc6\xf4\xf6\xf2\xfb\xf9\xff\xd6\xdc\xa2\xa3\xa5₧ƒ'
    '\xe1\xed\xf3\xfa\xf1\xd1\xaa\xba\xbf⌐\xac\xbd\xbc\xa1\xab\xbb'
    '░▒▓│┤╡╢╖╕╣║╗╝╜╛┐'
    '└┴┬├─┼╞╟╚╔╩╦╠═╬╧'
    '╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀'
    'α\xdfΓπΣσ\xb5τΦΘΩδ∞φε∩'
    '≡\xb1≥≤⌠⌡\xf7≈\xb0∙\xb7√ⁿ\xb2■\xa0'
)


def _decode_cp437(raw):
    try:
        return raw.decode('ascii')
    except UnicodeDecodeError:
        return ''.join(chr(b) if b < 128 else _CP437_HIGH[b - 128] for b in raw)


def _u16(b, i):
    return int.from_bytes(b[i:i + 2], 'little')


def _u32(b, i):
    return int.from_bytes(b[i:i + 4], 'little')


def _u64(b, i):
    return int.from_bytes(b[i:i + 8], 'little')


# -- decompression

_zlib_decompress = None
_importing_zlib = False


def _get_zlib_decompress():
    """zlib.decompress, imported on first use."""
    global _zlib_decompress, _importing_zlib
    if _zlib_decompress is not None:
        return _zlib_decompress
    if _importing_zlib:
        # zlib itself is being imported from a zip: cannot decompress.
        raise ZipImportError("can't decompress data; zlib not available")
    _importing_zlib = True
    try:
        from zlib import decompress
    except Exception:
        raise ZipImportError("can't decompress data; zlib not available")
    finally:
        _importing_zlib = False
    _zlib_decompress = decompress
    return decompress


_zstd_decompressor_class = None
_importing_zstd = False


def _get_zstd_decompressor_class():
    """_zstd.ZstdDecompressor, imported on first use."""
    global _zstd_decompressor_class, _importing_zstd
    if _zstd_decompressor_class is not None:
        return _zstd_decompressor_class
    if _importing_zstd:
        raise ZipImportError("can't decompress data; zstd not available")
    _importing_zstd = True
    try:
        from _zstd import ZstdDecompressor
    except Exception:
        raise ZipImportError("can't decompress data; zstd not available")
    finally:
        _importing_zstd = False
    _zstd_decompressor_class = ZstdDecompressor
    return ZstdDecompressor


def _zstd_decompress(data):
    """Decompress one or more concatenated Zstandard frames."""
    cls = _get_zstd_decompressor_class()
    from _zstd import get_frame_size, ZstdError
    view = memoryview(data)
    if not len(view):
        raise ZipImportError("can't decompress data; truncated Zstandard data")
    out = []
    pos = 0
    while pos < len(view):
        try:
            size = get_frame_size(view[pos:])
        except ZstdError as e:
            raise ZipImportError("can't decompress data; truncated or invalid "
                                 "Zstandard data") from e
        dec = cls()
        try:
            out.append(dec.decompress(view[pos:pos + size]))
        except ZstdError as e:
            raise ZipImportError("can't decompress data; invalid Zstandard "
                                 "data") from e
        if not dec.eof:
            raise ZipImportError("can't decompress data; truncated Zstandard data")
        pos += size
    return b''.join(out)


def _member_data(archive, entry):
    """Read and decompress the data of a toc entry."""
    datapath, compress, data_size, file_size, offset, _, _, _ = entry
    if data_size < 0:
        raise ZipImportError('negative data size')
    with _io.open_code(archive) as fp:
        try:
            fp.seek(offset)
        except OSError:
            raise ZipImportError(f"can't read Zip file: {archive!r}", path=archive)
        header = fp.read(30)
        if len(header) != 30:
            raise EOFError('EOF read where not expected')
        if header[:4] != _LOCAL_SIG:
            raise ZipImportError(f'bad local file header: {archive!r}', path=archive)
        skip = 30 + _u16(header, 26) + _u16(header, 28)
        try:
            fp.seek(offset + skip)
        except OSError:
            raise ZipImportError(f"can't read Zip file: {archive!r}", path=archive)
        raw = fp.read(data_size)
        if len(raw) != data_size:
            raise OSError("zipimport: can't read data")
    if compress == _STORED:
        return raw
    if compress == _DEFLATED:
        decompress = _get_zlib_decompress()
        return decompress(raw, -15)
    if compress == _ZSTANDARD:
        return _zstd_decompress(raw)
    raise ZipImportError(f"can't decompress data; unsupported compression "
                         f"method {compress}")


# -- code objects

def _same_mtime(t1, t2):
    # DOS times have a two second resolution.
    return abs(t1 - t2) <= 1


def _dos_to_unix(dosdate, dostime):
    return time.mktime((
        (dosdate >> 9) + 1980,
        (dosdate >> 5) & 0xF,
        dosdate & 0x1F,
        dostime >> 11,
        (dostime >> 5) & 0x3F,
        (dostime & 0x1F) * 2,
        -1, -1, -1))


def _source_mtime_and_size(importer, pyc_path):
    """mtime and size of the .py next to a .pyc member (0, 0 if none)."""
    try:
        assert pyc_path[-1:] in ('c', 'o')
        src = pyc_path[:-1]
        entry = importer._get_files()[src]
        if entry is None:
            return 0, 0
        _, _, _, size, _, dostime, dosdate, _ = entry
        return _dos_to_unix(dosdate, dostime), size
    except (KeyError, IndexError, TypeError, AssertionError):
        return 0, 0


def _source_of_pyc(importer, pyc_path):
    assert pyc_path[-1:] in ('c', 'o')
    src = pyc_path[:-1]
    entry = importer._get_files().get(src)
    if entry is None:
        return None
    return _member_data(importer.archive, entry)


def _code_from_pyc(importer, pathname, fullpath, fullname, data):
    """Validate a .pyc payload and unmarshal it; None if it is stale."""
    if _check_pyc(importer, fullpath, fullname, data) is None:
        return None
    code = marshal.loads(data[16:])
    if not isinstance(code, _bootstrap_external._code_type):
        raise TypeError(f'compiled module {pathname!r} is not a code object')
    return code


def _check_pyc(importer, fullpath, fullname, data):
    """True if the .pyc header is valid and matches its source (when the
    source is present); None if stale; ImportError if malformed."""
    details = {'name': fullname, 'path': fullpath}
    flags = _bootstrap_external._classify_pyc(data, fullname, details)
    if flags & 0b1:
        check_source = flags & 0b10 != 0
        mode = _imp.check_hash_based_pycs
        if mode != 'never' and (check_source or mode == 'always'):
            source = _source_of_pyc(importer, fullpath)
            if source is not None:
                source_hash = _imp.source_hash(
                    _bootstrap_external._RAW_MAGIC_NUMBER, source)
                _bootstrap_external._validate_hash_pyc(
                    data, source_hash, fullname, details)
    else:
        mtime, size = _source_mtime_and_size(importer, fullpath)
        if mtime:
            stored_mtime = int.from_bytes(data[8:12], 'little')
            stored_size = int.from_bytes(data[12:16], 'little')
            if (not _same_mtime(stored_mtime, int(mtime) & 0xFFFFFFFF)
                    or stored_size != (size & 0xFFFFFFFF)):
                _bootstrap._verbose_message(
                    'bytecode is stale for {!r}', fullname)
                return None
    return True


def _code_from_source(pathname, source):
    source = source.replace(b'\r\n', b'\n').replace(b'\r', b'\n')
    return compile(source, pathname, 'exec', dont_inherit=True)


def _load_code(importer, fullname):
    """Find and build the code of *fullname*: (code, is package, path)."""
    relpath = _module_relpath(importer, fullname)
    files = importer._get_files()
    failure = None
    for suffix, is_bytecode, is_pkg in _SEARCH_ORDER:
        candidate = relpath + suffix
        entry = files.get(candidate)
        if entry is None:
            continue
        modpath = entry[0]
        _bootstrap._verbose_message('trying {}{}{}', importer.archive, path_sep,
                                    candidate, verbosity=2)
        data = _member_data(importer.archive, entry)
        code = None
        if is_bytecode:
            try:
                code = _code_from_pyc(importer, modpath, candidate, fullname, data)
            except ImportError as exc:
                failure = exc
        else:
            code = _code_from_source(modpath, data)
        if code is None:
            continue
        return code, is_pkg, modpath
    if failure is not None:
        raise ZipImportError(f"module load failed: {failure}",
                             name=fullname) from failure
    raise ZipImportError(f"can't find module {fullname!r}", name=fullname)
