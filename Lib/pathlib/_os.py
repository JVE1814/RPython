"""Operating-system helpers shared by the pathlib classes.

* vfspath()/vfsopen(): string conversion and opening that work for both
  os.PathLike objects and the abstract path classes of pathlib.types
  (which provide __vfspath__, __open_reader__ and __open_writer__).
* copying helpers used by Path.copy() and Path.move().
* PathInfo implementations backing Path.info.
"""

import errno
import os
from io import TextIOWrapper, text_encoding
from stat import S_ISDIR, S_ISREG, S_ISLNK, S_IMODE

# errno values that mean "this kind of metadata cannot be copied here"
_METADATA_UNSUPPORTED = frozenset(
    {errno.EPERM, errno.ENOTSUP, errno.ENODATA, errno.EINVAL, errno.EACCES})
# errno values after which copy_file_range() falls back to read/write
_FAST_COPY_UNSUPPORTED = frozenset(
    {errno.ENOSYS, errno.EXDEV, errno.EINVAL, errno.EOPNOTSUPP,
     errno.ETXTBSY, errno.EBADF})


def vfspath(obj):
    """The string form of a path object (os.fspath() or __vfspath__())."""
    try:
        return os.fsdecode(obj)
    except TypeError:
        pass
    method = getattr(type(obj), '__vfspath__', None)
    if method is None:
        raise TypeError(f"expected JoinablePath object, not {type(obj).__name__}")
    return method(obj)


def _binary_stream(obj, mode):
    """Open `obj` in binary mode 'r', 'w', 'a', 'x', '+r' or '+w'."""
    kind = type(obj)
    if mode == 'r':
        hook = getattr(kind, '__open_reader__', None)
        if hook is not None:
            return hook(obj)
        action = 'reading'
        native = 'rb'
    elif mode in ('w', 'a', 'x'):
        hook = getattr(kind, '__open_writer__', None)
        if hook is not None:
            return hook(obj, mode)
        action = 'writing'
        native = mode + 'b'
    else:
        hook = getattr(kind, '__open_updater__', None)
        if hook is not None:
            return hook(obj, mode[1])
        action = 'updating'
        native = mode[1] + '+b'
    try:
        return open(obj, native)
    except TypeError:
        raise TypeError(f"{kind.__name__} can't be opened for {action}") from None


def vfsopen(obj, mode='r', buffering=-1, encoding=None, errors=None,
            newline=None):
    """Open a path object like open(), also accepting objects that only
    implement __open_reader__() / __open_writer__(mode).

    Custom buffering is not supported."""
    text = 'b' not in mode
    if text:
        encoding = text_encoding(encoding)
    if buffering != -1:
        raise ValueError("buffer size can't be customized")
    if not text:
        for value, what in ((encoding, "an encoding"), (errors, "an errors"),
                            (newline, "a newline")):
            if value is not None:
                raise ValueError(f"binary mode doesn't take {what} argument")
    basic = ''.join(sorted(c for c in mode if c not in 'bt'))
    if basic not in ('r', 'w', 'a', 'x', '+r', '+w'):
        raise ValueError(f'invalid mode: {basic!r}')
    stream = _binary_stream(obj, basic)
    if text:
        stream = TextIOWrapper(stream, encoding, errors, newline)
    return stream


def magic_open(path, mode='r', buffering=-1, encoding=None, errors=None,
               newline=None):
    """open() for os.PathLike objects, vfsopen() for other path objects."""
    if 'b' not in mode:
        encoding = text_encoding(encoding)
    try:
        return open(path, mode, buffering, encoding, errors, newline)
    except TypeError:
        pass
    return vfsopen(path, mode, buffering, encoding, errors, newline)


def _same_path_error(message, source, target):
    err = OSError(errno.EINVAL, message)
    err.filename = vfspath(source)
    err.filename2 = vfspath(target)
    return err


def ensure_distinct_paths(source, target):
    """Refuse to copy a path onto itself or into one of its descendants
    (a lexical check)."""
    if source == target:
        raise _same_path_error("Source and target are the same path", source, target)
    if source in target.parents:
        raise _same_path_error("Source path is a parent of target path", source, target)


def ensure_different_files(source, target):
    """Refuse to copy when both paths name the same existing file."""
    try:
        source_id = source.info._file_id
        target_id = target.info._file_id
    except AttributeError:
        same = source == target
    else:
        try:
            same = source_id() == target_id()
        except (OSError, ValueError):
            same = False
    if same:
        raise _same_path_error("Source and target are the same file", source, target)


def copyfileobj(source_f, target_f):
    """Copy all data between two binary files, using copy_file_range()
    when both are real files."""
    try:
        src_fd = source_f.fileno()
        dst_fd = target_f.fileno()
    except Exception:
        src_fd = dst_fd = None
    if src_fd is not None and hasattr(os, 'copy_file_range'):
        try:
            block = max(os.fstat(src_fd).st_blksize, 1 << 16)
        except OSError:
            block = 1 << 16
        try:
            while os.copy_file_range(src_fd, dst_fd, block):
                pass
            return
        except OSError as err:
            err.filename = source_f.name
            err.filename2 = target_f.name
            if err.errno not in _FAST_COPY_UNSUPPORTED:
                raise err
    while chunk := source_f.read(1 << 20):
        target_f.write(chunk)


def copy_info(info, target, follow_symlinks=True):
    """Copy times, extended attributes and permissions described by the
    PathInfo `info` to the local path `target` (best effort)."""
    def supported(func):
        return follow_symlinks or func in os.supports_follow_symlinks

    if hasattr(info, '_access_time_ns') and hasattr(info, '_mod_time_ns') \
            and supported(os.utime):
        times = (info._access_time_ns(follow_symlinks=follow_symlinks),
                 info._mod_time_ns(follow_symlinks=follow_symlinks))
        os.utime(target, ns=times, follow_symlinks=follow_symlinks)

    # extended attributes go first: chmod() may make the target read-only
    if hasattr(info, '_xattrs') and hasattr(os, 'setxattr') and supported(os.setxattr):
        for name, value in info._xattrs(follow_symlinks=follow_symlinks):
            try:
                os.setxattr(target, name, value, follow_symlinks=follow_symlinks)
            except OSError as err:
                if err.errno not in _METADATA_UNSUPPORTED:
                    raise

    if hasattr(info, '_posix_permissions') and supported(os.chmod):
        mode = info._posix_permissions(follow_symlinks=follow_symlinks)
        try:
            os.chmod(target, mode, follow_symlinks=follow_symlinks)
        except NotImplementedError:
            # no way to chmod a symlink on this platform
            pass

    if hasattr(info, '_bsd_flags') and hasattr(os, 'chflags') and supported(os.chflags):
        flags = info._bsd_flags(follow_symlinks=follow_symlinks)
        try:
            os.chflags(target, flags, follow_symlinks=follow_symlinks)
        except OSError as err:
            if err.errno not in (errno.EOPNOTSUPP, errno.ENOTSUP):
                raise


_UNKNOWN = object()


class _StatInfo:
    """PathInfo for a local path, caching stat() and lstat() results."""
    __slots__ = ('_path', '_stat_cache', '_lstat_cache')

    def __init__(self, path):
        self._path = str(path)
        self._stat_cache = _UNKNOWN
        self._lstat_cache = _UNKNOWN

    def __repr__(self):
        kind = "WindowsPath" if os.name == "nt" else "PosixPath"
        return f"<{kind}.info>"

    def _stat(self, *, follow_symlinks=True, ignore_errors=False):
        attr = '_stat_cache' if follow_symlinks else '_lstat_cache'
        cached = getattr(self, attr)
        if cached is not _UNKNOWN and (cached is not None or ignore_errors):
            return cached
        try:
            result = os.stat(self._path) if follow_symlinks else os.lstat(self._path)
        except (OSError, ValueError):
            setattr(self, attr, None)
            if ignore_errors:
                return None
            raise
        setattr(self, attr, result)
        return result

    def _mode(self, follow_symlinks):
        st = self._stat(follow_symlinks=follow_symlinks, ignore_errors=True)
        return None if st is None else st.st_mode

    def exists(self, *, follow_symlinks=True):
        """True if the path exists."""
        return self._mode(follow_symlinks) is not None

    def is_dir(self, *, follow_symlinks=True):
        """True if the path is a directory."""
        mode = self._mode(follow_symlinks)
        return mode is not None and S_ISDIR(mode)

    def is_file(self, *, follow_symlinks=True):
        """True if the path is a regular file."""
        mode = self._mode(follow_symlinks)
        return mode is not None and S_ISREG(mode)

    def is_symlink(self):
        """True if the path is a symbolic link."""
        mode = self._mode(False)
        return mode is not None and S_ISLNK(mode)

    # metadata accessors used by copy_info() and ensure_different_files()

    def _posix_permissions(self, *, follow_symlinks=True):
        return S_IMODE(self._stat(follow_symlinks=follow_symlinks).st_mode)

    def _file_id(self, *, follow_symlinks=True):
        st = self._stat(follow_symlinks=follow_symlinks)
        return st.st_dev, st.st_ino

    def _access_time_ns(self, *, follow_symlinks=True):
        return self._stat(follow_symlinks=follow_symlinks).st_atime_ns

    def _mod_time_ns(self, *, follow_symlinks=True):
        return self._stat(follow_symlinks=follow_symlinks).st_mtime_ns

    if hasattr(os.stat_result, 'st_flags'):
        def _bsd_flags(self, *, follow_symlinks=True):
            return self._stat(follow_symlinks=follow_symlinks).st_flags

    if hasattr(os, 'listxattr'):
        def _xattrs(self, *, follow_symlinks=True):
            try:
                names = os.listxattr(self._path, follow_symlinks=follow_symlinks)
                return [(name, os.getxattr(self._path, name,
                                           follow_symlinks=follow_symlinks))
                        for name in names]
            except OSError as err:
                if err.errno not in _METADATA_UNSUPPORTED:
                    raise
                return []


PathInfo = _StatInfo


class DirEntryInfo(_StatInfo):
    """PathInfo answering from an os.DirEntry produced by scandir()."""
    __slots__ = ('_entry',)

    def __init__(self, entry):
        super().__init__(entry.path)
        self._entry = entry

    def _stat(self, *, follow_symlinks=True, ignore_errors=False):
        try:
            return self._entry.stat(follow_symlinks=follow_symlinks)
        except OSError:
            if ignore_errors:
                return None
            raise

    def exists(self, *, follow_symlinks=True):
        """True if the path exists."""
        if not follow_symlinks:
            return True   # scandir() found it
        return self._stat(ignore_errors=True) is not None

    def is_dir(self, *, follow_symlinks=True):
        """True if the path is a directory."""
        try:
            return self._entry.is_dir(follow_symlinks=follow_symlinks)
        except OSError:
            return False

    def is_file(self, *, follow_symlinks=True):
        """True if the path is a regular file."""
        try:
            return self._entry.is_file(follow_symlinks=follow_symlinks)
        except OSError:
            return False

    def is_symlink(self):
        """True if the path is a symbolic link."""
        try:
            return self._entry.is_symlink()
        except OSError:
            return False
