"""High-level file operations: copying, moving and removing files and
directory trees, archive creation/extraction, and a few related helpers
(which, disk_usage, chown, get_terminal_size).
"""

import collections
import errno
import fnmatch
import os
import stat
import sys

try:
    import zlib
    del zlib
    _ZLIB_SUPPORTED = True
except ImportError:
    _ZLIB_SUPPORTED = False

try:
    import bz2
    del bz2
    _BZ2_SUPPORTED = True
except ImportError:
    _BZ2_SUPPORTED = False

try:
    import lzma
    del lzma
    _LZMA_SUPPORTED = True
except ImportError:
    _LZMA_SUPPORTED = False

try:
    from compression import zstd
    del zstd
    _ZSTD_SUPPORTED = True
except ImportError:
    _ZSTD_SUPPORTED = False

try:
    import posix
except ImportError:
    posix = None

_WINDOWS = os.name == "nt"
if _WINDOWS:
    import nt
    try:
        import _winapi
    except ImportError:
        _winapi = None
else:
    nt = None
    _winapi = None

# Chunk size used by copyfileobj() when no length is given.
COPY_BUFSIZE = 1024 * 1024 if _WINDOWS else 64 * 1024

# Zero-copy strategies; the flags are module globals so that a failed
# probe (or a test) can switch a strategy off.
_USE_CP_SENDFILE = (hasattr(os, "sendfile")
                    and sys.platform.startswith(("linux", "android", "sunos")))
_USE_CP_COPY_FILE_RANGE = hasattr(os, "copy_file_range")
_HAS_FCOPYFILE = posix is not None and hasattr(posix, "_fcopyfile")

__all__ = ["copyfileobj", "copyfile", "copymode", "copystat", "copy", "copy2",
           "copytree", "move", "rmtree", "Error", "SpecialFileError",
           "make_archive", "get_archive_formats",
           "register_archive_format", "unregister_archive_format",
           "get_unpack_formats", "register_unpack_format",
           "unregister_unpack_format", "unpack_archive",
           "ignore_patterns", "chown", "which", "get_terminal_size",
           "SameFileError"]


# --------------------------------------------------------------------------
# Exceptions

class Error(OSError):
    pass

class SameFileError(Error):
    """Source and destination are the same file."""

class SpecialFileError(OSError):
    """Raised for an operation that is not supported on a special file
    (for example a named pipe)."""

class ReadError(OSError):
    """An archive could not be read."""

class RegistryError(Exception):
    """Conflicting registration in the archive format registries."""

class _GiveupOnFastCopy(Exception):
    """A zero-copy primitive is not usable for this pair of files; the
    caller should fall back to a plain read/write loop."""


class ExecError(OSError):
    """Raised when a command could not be executed."""

_ExecError = ExecError
del ExecError

def __getattr__(name):
    if name == "ExecError":
        import warnings
        warnings._deprecated("shutil.ExecError", remove=(3, 16))
        return _ExecError
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


# --------------------------------------------------------------------------
# Copying file contents

def _fileno_pair(fsrc, fdst):
    try:
        return fsrc.fileno(), fdst.fileno()
    except Exception as exc:
        raise _GiveupOnFastCopy(exc)


def _zero_copy_loop(fsrc, fdst, outfd, step, chunk):
    """Drive a kernel copy primitive until it reports end of file.

    *step(offset, chunk)* copies at most *chunk* bytes and returns how many
    were copied.  Failures before anything was written turn into
    _GiveupOnFastCopy so the caller can fall back to a userspace copy.
    """
    offset = 0
    while True:
        try:
            done = step(offset, chunk)
        except OSError as exc:
            exc.filename = getattr(fsrc, "name", None)
            exc.filename2 = getattr(fdst, "name", None)
            if exc.errno == errno.ENOSPC:
                # Out of space: retrying with read()/write() cannot help.
                raise exc from None
            if offset == 0 and os.lseek(outfd, 0, os.SEEK_CUR) == 0:
                raise _GiveupOnFastCopy(exc)
            raise exc
        if done == 0:
            return offset
        offset += done


def _chunk_size_for(infd):
    # Ask for the whole file in one call where possible; the kernel caps
    # the transfer anyway.  Unknown sizes use a generous fixed chunk.
    try:
        size = os.fstat(infd).st_size
    except OSError:
        size = 0
        chunk = 2 ** 27
    else:
        chunk = max(size, 2 ** 23)
    if sys.maxsize < 2 ** 32:
        chunk = min(chunk, 2 ** 30)
    return chunk


def _fastcopy_sendfile(fsrc, fdst):
    """Copy between two regular files using os.sendfile()."""
    global _USE_CP_SENDFILE
    infd, outfd = _fileno_pair(fsrc, fdst)
    chunk = _chunk_size_for(infd)

    def step(offset, count):
        global _USE_CP_SENDFILE
        try:
            return os.sendfile(outfd, infd, offset, count)
        except OSError as exc:
            if exc.errno == errno.ENOTSOCK:
                # This kernel only supports socket destinations.
                _USE_CP_SENDFILE = False
                raise _GiveupOnFastCopy(exc)
            raise

    _zero_copy_loop(fsrc, fdst, outfd, step, chunk)


def _fastcopy_copy_file_range(fsrc, fdst):
    """Copy between two regular files using os.copy_file_range()."""
    infd, outfd = _fileno_pair(fsrc, fdst)
    chunk = _chunk_size_for(infd)

    def step(offset, count):
        return os.copy_file_range(infd, outfd, count)

    copied = _zero_copy_loop(fsrc, fdst, outfd, step, chunk)
    if copied == 0:
        # Some filesystems report 0 for files whose size they do not know
        # (procfs and friends); let the generic loop handle them.
        raise _GiveupOnFastCopy()


def _fastcopy_fcopyfile(fsrc, fdst, flags):
    """macOS: copy using fcopyfile(3)."""
    infd, outfd = _fileno_pair(fsrc, fdst)
    try:
        posix._fcopyfile(infd, outfd, flags)
    except OSError as exc:
        exc.filename = getattr(fsrc, "name", None)
        exc.filename2 = getattr(fdst, "name", None)
        if exc.errno in {errno.EINVAL, errno.ENOTSUP}:
            raise _GiveupOnFastCopy(exc)
        raise exc from None


def _copyfileobj_readinto(fsrc, fdst, length=COPY_BUFSIZE):
    """Copy using readinto() into a reusable buffer (used on Windows)."""
    readinto = fsrc.readinto
    write = fdst.write
    with memoryview(bytearray(length)) as buf:
        while True:
            n = readinto(buf)
            if not n:
                return
            if n < length:
                with buf[:n] as part:
                    write(part)
            else:
                write(buf)


def copyfileobj(fsrc, fdst, length=0):
    """Copy the remaining data of file object fsrc to file object fdst."""
    if not length:
        length = COPY_BUFSIZE
    read = fsrc.read
    write = fdst.write
    while True:
        data = read(length)
        if not data:
            break
        write(data)


def _samefile(src, dst):
    if isinstance(src, os.DirEntry) and hasattr(os.path, "samestat"):
        try:
            return os.path.samestat(src.stat(), os.stat(dst))
        except OSError:
            return False
    if hasattr(os.path, "samefile"):
        try:
            return os.path.samefile(src, dst)
        except OSError:
            return False
    return (os.path.normcase(os.path.abspath(src)) ==
            os.path.normcase(os.path.abspath(dst)))


def _stat(fn):
    return fn.stat() if isinstance(fn, os.DirEntry) else os.stat(fn)


def _islink(fn):
    return fn.is_symlink() if isinstance(fn, os.DirEntry) else os.path.islink(fn)


def _display_path(fn):
    return fn.path if isinstance(fn, os.DirEntry) else fn


def _fast_copy_contents(fsrc, fdst, size_hint):
    """Try the platform's zero-copy primitives; return True on success."""
    if _HAS_FCOPYFILE:
        try:
            _fastcopy_fcopyfile(fsrc, fdst, posix._COPYFILE_DATA)
            return True
        except _GiveupOnFastCopy:
            return False
    if _USE_CP_COPY_FILE_RANGE:
        try:
            _fastcopy_copy_file_range(fsrc, fdst)
            return True
        except _GiveupOnFastCopy:
            pass
    if _USE_CP_SENDFILE:
        try:
            _fastcopy_sendfile(fsrc, fdst)
            return True
        except _GiveupOnFastCopy:
            pass
    if _WINDOWS and size_hint > 0:
        _copyfileobj_readinto(fsrc, fdst, min(size_hint, COPY_BUFSIZE))
        return True
    return False


def copyfile(src, dst, *, follow_symlinks=True):
    """Copy the contents (no metadata) of file src to file dst.

    Returns dst.  If follow_symlinks is false and src is a symbolic link,
    a new link is created instead of copying what it points to.
    """
    sys.audit("shutil.copyfile", src, dst)

    if _samefile(src, dst):
        raise SameFileError("{!r} and {!r} are the same file".format(src, dst))

    size_hint = 0
    for index, fn in enumerate((src, dst)):
        try:
            st = _stat(fn)
        except OSError:
            continue
        if stat.S_ISFIFO(st.st_mode):
            raise SpecialFileError("`%s` is a named pipe" % _display_path(fn))
        if index == 0:
            size_hint = st.st_size

    if not follow_symlinks and _islink(src):
        os.symlink(os.readlink(src), dst)
        return dst

    with open(src, "rb") as fsrc:
        try:
            with open(dst, "wb") as fdst:
                if not _fast_copy_contents(fsrc, fdst, size_hint):
                    copyfileobj(fsrc, fdst)
        except IsADirectoryError as exc:
            # Opening "missing/" for writing reports EISDIR on some
            # systems; say what really went wrong.
            if not os.path.exists(dst):
                raise FileNotFoundError(f"Directory does not exist: {dst}") from exc
            raise
    return dst


# --------------------------------------------------------------------------
# Copying metadata

def copymode(src, dst, *, follow_symlinks=True):
    """Copy the permission bits of src to dst."""
    sys.audit("shutil.copymode", src, dst)

    if not follow_symlinks and _islink(src) and os.path.islink(dst):
        if not hasattr(os, "lchmod"):
            return
        if isinstance(src, os.DirEntry):
            st = src.stat(follow_symlinks=False)
        else:
            st = os.lstat(src)
        os.lchmod(dst, stat.S_IMODE(st.st_mode))
        return

    st = _stat(src)
    if _WINDOWS and os.path.islink(dst):
        dst = os.path.realpath(dst, strict=True)
    os.chmod(dst, stat.S_IMODE(st.st_mode))


if hasattr(os, "listxattr"):
    _XATTR_SOURCE_IGNORED = {errno.ENOTSUP, errno.ENODATA, errno.EINVAL}
    _XATTR_TARGET_IGNORED = {errno.EPERM, errno.ENOTSUP, errno.ENODATA,
                             errno.EINVAL, errno.EACCES}

    def _copyxattr(src, dst, *, follow_symlinks=True):
        """Copy extended filesystem attributes from src to dst.

        Attributes the destination refuses are skipped silently.
        """
        try:
            names = os.listxattr(src, follow_symlinks=follow_symlinks)
        except OSError as exc:
            if exc.errno not in _XATTR_SOURCE_IGNORED:
                raise
            return
        for attr in names:
            try:
                value = os.getxattr(src, attr, follow_symlinks=follow_symlinks)
                os.setxattr(dst, attr, value, follow_symlinks=follow_symlinks)
            except OSError as exc:
                if exc.errno not in _XATTR_TARGET_IGNORED:
                    raise
else:
    def _copyxattr(*args, **kwargs):
        pass


def _ignored_chflags_error(exc):
    harmless = {getattr(errno, name) for name in ("EOPNOTSUPP", "ENOTSUP")
                if hasattr(errno, name)}
    return exc.errno in harmless


def copystat(src, dst, *, follow_symlinks=True):
    """Copy permission bits, timestamps, flags and extended attributes
    from src to dst (not the owner).

    With follow_symlinks false and both arguments symlinks, the links
    themselves are updated where the platform allows it.
    """
    sys.audit("shutil.copystat", src, dst)

    follow = follow_symlinks or not (_islink(src) and os.path.islink(dst))

    def os_call(name):
        func = getattr(os, name, None)
        if func is None:
            return None
        if not follow and func not in os.supports_follow_symlinks:
            return None
        return func

    if isinstance(src, os.DirEntry):
        st = src.stat(follow_symlinks=follow)
    else:
        st = os.stat(src, follow_symlinks=follow)

    utime = os_call("utime")
    if utime is not None:
        utime(dst, ns=(st.st_atime_ns, st.st_mtime_ns), follow_symlinks=follow)

    # Extended attributes go first: a read-only mode set below could
    # otherwise prevent writing them.
    _copyxattr(src, dst, follow_symlinks=follow)

    chmod = os_call("chmod")
    if chmod is not None:
        try:
            chmod(dst, stat.S_IMODE(st.st_mode), follow_symlinks=follow)
        except NotImplementedError:
            # Asked to change a symlink's mode on a platform that cannot.
            pass

    flags = getattr(st, "st_flags", None)
    if flags is not None:
        chflags = os_call("chflags")
        if chflags is not None:
            try:
                chflags(dst, flags, follow_symlinks=follow)
            except OSError as exc:
                if not _ignored_chflags_error(exc):
                    raise


def copy(src, dst, *, follow_symlinks=True):
    """Copy file data and permission bits; dst may be a directory.

    Returns the path of the new file.
    """
    if os.path.isdir(dst):
        dst = os.path.join(dst, os.path.basename(src))
    copyfile(src, dst, follow_symlinks=follow_symlinks)
    copymode(src, dst, follow_symlinks=follow_symlinks)
    return dst


def copy2(src, dst, *, follow_symlinks=True):
    """Copy file data and all metadata copystat() handles; dst may be a
    directory.  Returns the path of the new file."""
    if os.path.isdir(dst):
        dst = os.path.join(dst, os.path.basename(src))
    copyfile(src, dst, follow_symlinks=follow_symlinks)
    copystat(src, dst, follow_symlinks=follow_symlinks)
    return dst


# --------------------------------------------------------------------------
# Trees

def ignore_patterns(*patterns):
    """Return an ignore callable for copytree() that skips names matching
    any of the glob-style *patterns*."""
    def _ignore(path, names):
        skipped = set()
        for pattern in patterns:
            skipped.update(fnmatch.filter(names, pattern))
        return skipped
    return _ignore


def _copy_tree_entries(entries, src, dst, symlinks, ignore, copy_function,
                       ignore_dangling_symlinks, dirs_exist_ok):
    if ignore is not None:
        skip = set(ignore(os.fspath(src), [entry.name for entry in entries]))
    else:
        skip = ()

    os.makedirs(dst, exist_ok=dirs_exist_ok)
    failures = []
    # copy() and copy2() accept DirEntry objects and can use their cached
    # stat data; arbitrary callables get plain paths.
    pass_entries = copy_function is copy2 or copy_function is copy

    for entry in entries:
        if entry.name in skip:
            continue
        srcname = os.path.join(src, entry.name)
        dstname = os.path.join(dst, entry.name)
        source = entry if pass_entries else srcname
        try:
            is_link = entry.is_symlink()
            if is_link and _WINDOWS:
                # Junctions behave like symlinks for our purposes.
                st = entry.stat(follow_symlinks=False)
                if st.st_reparse_tag == stat.IO_REPARSE_TAG_MOUNT_POINT:
                    is_link = False
            if is_link:
                target = os.readlink(srcname)
                if symlinks:
                    os.symlink(target, dstname)
                    copystat(source, dstname, follow_symlinks=False)
                    continue
                if not os.path.exists(target) and ignore_dangling_symlinks:
                    continue
            if entry.is_dir():
                copytree(source, dstname, symlinks, ignore, copy_function,
                         ignore_dangling_symlinks, dirs_exist_ok)
            else:
                copy_function(source, dstname)
        except Error as exc:
            failures.extend(exc.args[0])
        except OSError as exc:
            failures.append((srcname, dstname, str(exc)))

    try:
        copystat(src, dst)
    except OSError as exc:
        # Windows refuses some attribute changes on certain filesystems
        # (FAT); those are not worth reporting.
        if getattr(exc, "winerror", None) is None:
            failures.append((src, dst, str(exc)))
    if failures:
        raise Error(failures)
    return dst


def copytree(src, dst, symlinks=False, ignore=None, copy_function=copy2,
             ignore_dangling_symlinks=False, dirs_exist_ok=False):
    """Recursively copy the directory tree at src to dst.

    symlinks: copy symbolic links as links (True) or what they point to.
    ignore: callable(dir, names) -> names to skip (see ignore_patterns()).
    copy_function: used for each file (default copy2).
    ignore_dangling_symlinks: silently skip broken links when symlinks
        is false.
    dirs_exist_ok: do not fail if dst (or a subdirectory) already exists.

    Errors are collected and raised together as Error(list of
    (src, dst, reason)) after everything possible has been copied.
    Returns dst.
    """
    sys.audit("shutil.copytree", src, dst)
    with os.scandir(src) as it:
        entries = list(it)
    return _copy_tree_entries(entries, src, dst, symlinks, ignore,
                              copy_function, ignore_dangling_symlinks,
                              dirs_exist_ok)


# -- rmtree

_use_fd_functions = (
    {os.open, os.stat, os.unlink, os.rmdir} <= os.supports_dir_fd
    and os.scandir in os.supports_fd
    and os.stat in os.supports_follow_symlinks)

_SYMLINK_MESSAGE = "Cannot call rmtree on a symbolic link"


def _make_reporter(onexc, pass_kwargs):
    if pass_kwargs:
        def report(func, path, exc, direntry=None, dir_fd=None):
            onexc(func, path, exc, direntry=direntry, dir_fd=dir_fd)
    else:
        def report(func, path, exc, direntry=None, dir_fd=None):
            onexc(func, path, exc)
    return report


def _is_junction(st):
    tag = getattr(st, "st_reparse_tag", None)
    return tag is not None and tag == getattr(stat, "IO_REPARSE_TAG_MOUNT_POINT", None)


def _not_a_directory(path):
    return NotADirectoryError(errno.ENOTDIR, os.strerror(errno.ENOTDIR), path)


class _OpenDir:
    """One directory being emptied by the fd based rmtree walk."""
    __slots__ = ("fd", "path", "entry", "entries")

    def __init__(self, fd, path, entry):
        self.fd = fd
        self.path = path
        self.entry = entry
        self.entries = None


def _rmtree_with_fds(path, dir_fd, report):
    """rmtree() using *at() system calls on open directory descriptors, so
    that symlinks swapped in during the walk are never followed.  The walk
    uses an explicit stack (no recursion limit issues)."""
    as_bytes = isinstance(path, bytes)
    try:
        top_st = os.lstat(path, dir_fd=dir_fd)
    except OSError as exc:
        report(os.lstat, path, exc)
        return

    if stat.S_ISLNK(top_st.st_mode) or _is_junction(top_st):
        try:
            raise OSError(_SYMLINK_MESSAGE)
        except OSError as exc:
            report(os.path.islink, path, exc)
        return

    if not stat.S_ISDIR(top_st.st_mode):
        report(os.scandir, path, _not_a_directory(path))
        try:
            os.rmdir(path, dir_fd=dir_fd)
        except OSError as exc:
            report(os.rmdir, path, exc)
        return

    open_flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_CLOEXEC", 0)
    try:
        top_fd = os.open(path, open_flags, dir_fd=dir_fd)
    except OSError as exc:
        report(os.open, path, exc)
        return

    stack = []
    try:
        try:
            same = os.path.samestat(top_st, os.fstat(top_fd))
        except OSError:
            same = False
        if not same:
            # Replaced by something else (e.g. a symlink) after the lstat.
            fd, top_fd = top_fd, None
            os.close(fd)
            try:
                raise OSError(_SYMLINK_MESSAGE)
            except OSError as exc:
                report(os.path.islink, path, exc)
            return
        stack.append(_OpenDir(top_fd, path, None))
        top_fd = None

        while stack:
            current = stack[-1]
            if current.entries is None:
                try:
                    with os.scandir(current.fd) as it:
                        listing = list(it)
                except OSError as exc:
                    exc.filename = current.path
                    report(os.scandir, current.path, exc)
                    listing = []
                current.entries = iter(listing)

            descended = False
            for entry in current.entries:
                name = os.fsencode(entry.name) if as_bytes else entry.name
                fullname = os.path.join(current.path, name)
                try:
                    is_dir = entry.is_dir(follow_symlinks=False)
                except OSError:
                    is_dir = False
                if is_dir:
                    try:
                        entry_st = entry.stat(follow_symlinks=False)
                    except FileNotFoundError:
                        continue
                    except OSError as exc:
                        report(os.lstat, fullname, exc, entry, current.fd)
                        continue
                    if stat.S_ISDIR(entry_st.st_mode):
                        try:
                            child_fd = os.open(entry.name, open_flags,
                                               dir_fd=current.fd)
                        except FileNotFoundError:
                            continue
                        except OSError as exc:
                            report(os.open, fullname, exc, entry, current.fd)
                            continue
                        try:
                            same = os.path.samestat(entry_st, os.fstat(child_fd))
                        except OSError:
                            same = False
                        if same:
                            stack.append(_OpenDir(child_fd, fullname, entry))
                            descended = True
                            break
                        os.close(child_fd)
                        try:
                            raise OSError(_SYMLINK_MESSAGE)
                        except OSError as exc:
                            report(os.path.islink, fullname, exc, entry,
                                   current.fd)
                        continue
                try:
                    os.unlink(entry.name, dir_fd=current.fd)
                except FileNotFoundError:
                    continue
                except OSError as exc:
                    report(os.unlink, fullname, exc, entry, current.fd)
            if descended:
                continue

            # Directory emptied (as far as possible): close and remove it.
            stack.pop()
            try:
                os.close(current.fd)
            except OSError as exc:
                report(os.close, current.path, exc)
            if stack:
                parent = stack[-1]
                try:
                    os.rmdir(current.entry.name, dir_fd=parent.fd)
                except FileNotFoundError:
                    pass
                except OSError as exc:
                    report(os.rmdir, current.path, exc, current.entry, parent.fd)
    finally:
        # Only reached with open descriptors if a handler raised.
        if top_fd is not None:
            try:
                os.close(top_fd)
            except OSError:
                pass
        while stack:
            leftover = stack.pop()
            try:
                os.close(leftover.fd)
            except OSError:
                pass

    try:
        os.rmdir(path, dir_fd=dir_fd)
    except FileNotFoundError:
        pass
    except OSError as exc:
        report(os.rmdir, path, exc)


def _rmtree_with_paths(path, report):
    """Portable rmtree() working on path names (not symlink-race safe)."""
    try:
        top_st = os.lstat(path)
    except OSError as exc:
        report(os.lstat, path, exc)
        return
    if stat.S_ISLNK(top_st.st_mode) or _is_junction(top_st):
        try:
            raise OSError(_SYMLINK_MESSAGE)
        except OSError as exc:
            report(os.path.islink, path, exc)
        return

    # Each stack item: [path, iterator of entries or None]
    stack = [[path, None]]
    while stack:
        item = stack[-1]
        current, pending = item
        if pending is None:
            try:
                with os.scandir(current) as it:
                    listing = list(it)
            except OSError as exc:
                report(os.scandir, current, exc)
                listing = []
            pending = item[1] = iter(listing)
        descended = False
        for entry in pending:
            fullname = entry.path
            try:
                is_dir = entry.is_dir(follow_symlinks=False)
            except OSError:
                is_dir = False
            if is_dir and _WINDOWS:
                try:
                    if _is_junction(entry.stat(follow_symlinks=False)):
                        is_dir = False
                except OSError:
                    pass
            if is_dir:
                stack.append([fullname, None])
                descended = True
                break
            try:
                os.unlink(fullname)
            except FileNotFoundError:
                continue
            except OSError as exc:
                report(os.unlink, fullname, exc)
        if descended:
            continue
        stack.pop()
        try:
            os.rmdir(current)
        except FileNotFoundError:
            pass
        except OSError as exc:
            report(os.rmdir, current, exc)


def rmtree(path, ignore_errors=False, onerror=None, *, onexc=None,
           dir_fd=None, _onexc_kwargs=False):
    """Delete a directory tree; symbolic links inside it are removed, not
    followed, and rmtree refuses to start at a symbolic link.

    ignore_errors: swallow all errors.
    onexc: callable(function, path, exception) called for each failure.
    onerror: deprecated variant receiving sys.exc_info()-style triples;
        ignored when onexc is given.
    dir_fd: path is relative to this directory descriptor (only where
        rmtree.avoids_symlink_attacks is true).
    Without a handler the first error is raised.
    """
    sys.audit("shutil.rmtree", path, dir_fd)
    if ignore_errors:
        def onexc(*args, **kwargs):
            pass
        _onexc_kwargs = True
    elif onexc is None:
        if onerror is not None:
            legacy = onerror

            def onexc(func, path, exc):
                legacy(func, path, (type(exc), exc, exc.__traceback__))
        else:
            def onexc(func, path, exc, **kwargs):
                raise exc
            _onexc_kwargs = True

    report = _make_reporter(onexc, _onexc_kwargs)
    if _use_fd_functions:
        _rmtree_with_fds(path, dir_fd, report)
    else:
        if dir_fd is not None:
            raise NotImplementedError("dir_fd unavailable on this platform")
        _rmtree_with_paths(path, report)

# Lets callers (and tests) know whether the race-free variant is in use.
rmtree.avoids_symlink_attacks = _use_fd_functions


# -- move

def _basename(path):
    """basename() that ignores trailing separators."""
    path = os.fspath(path)
    seps = os.path.sep + (os.path.altsep or "")
    if isinstance(path, bytes):
        seps = os.fsencode(seps)
    return os.path.basename(path.rstrip(seps))


def _destinsrc(src, dst):
    """True if dst lies inside the directory src (symlinks resolved)."""
    src = os.path.realpath(src)
    dst = os.path.realpath(dst)
    sep = os.path.sep if isinstance(src, str) else os.fsencode(os.path.sep)
    if not src.endswith(sep):
        src += sep
    if not dst.endswith(sep):
        dst += sep
    return dst.startswith(src)


def _is_immutable(src):
    st = _stat(src)
    flags = getattr(st, "st_flags", None)
    if flags is None:
        return False
    immutable = {getattr(stat, name, None) for name in ("UF_IMMUTABLE", "SF_IMMUTABLE")}
    immutable.discard(None)
    return flags in immutable


def move(src, dst, copy_function=copy2):
    """Move a file or directory tree to another location, like mv(1).

    If dst is an existing directory, src is moved inside it.  os.rename()
    is used when possible; otherwise the data is copied (copy_function for
    files, copytree() for directories) and the source removed.
    Returns the final destination path.
    """
    sys.audit("shutil.move", src, dst)
    target = dst
    if os.path.isdir(dst):
        if _samefile(src, dst) and not os.path.islink(src):
            # Same directory under a different spelling (for example a
            # case-only rename on a case-insensitive filesystem).
            os.rename(src, dst)
            return dst
        target = os.path.join(dst, _basename(src))
        if os.path.exists(target):
            raise Error("Destination path '%s' already exists" % target)
    try:
        os.rename(src, target)
        return target
    except OSError:
        pass

    if os.path.islink(src):
        os.symlink(os.readlink(src), target)
        os.unlink(src)
    elif os.path.isdir(src):
        if _destinsrc(src, dst):
            raise Error("Cannot move a directory '%s' into itself '%s'."
                        % (src, dst))
        if (_is_immutable(src)
                or (not os.access(src, os.W_OK) and os.listdir(src)
                    and sys.platform == "darwin")):
            raise PermissionError("Cannot move the non-empty directory "
                                  "'%s': Lacking write permission to '%s'."
                                  % (src, src))
        copytree(src, target, copy_function=copy_function, symlinks=True)
        rmtree(src)
    else:
        copy_function(src, target)
        os.unlink(src)
    return target


# --------------------------------------------------------------------------
# Disk usage, ownership, terminal size

if hasattr(os, "statvfs"):
    _ntuple_diskusage = collections.namedtuple("usage", "total used free")

    def disk_usage(path):
        """Return (total, used, free) bytes of the filesystem holding path."""
        info = os.statvfs(path)
        unit = info.f_frsize
        total = info.f_blocks * unit
        free = info.f_bavail * unit
        used = (info.f_blocks - info.f_bfree) * unit
        return _ntuple_diskusage(total, used, free)

    __all__.append("disk_usage")

elif _WINDOWS:
    _ntuple_diskusage = collections.namedtuple("usage", "total used free")

    def disk_usage(path):
        """Return (total, used, free) bytes of the filesystem holding path."""
        total, free = nt._getdiskusage(path)
        return _ntuple_diskusage(total, total - free, free)

    __all__.append("disk_usage")


def _get_gid(name):
    """Group id for a group name, or None."""
    if name is None:
        return None
    try:
        import grp
    except ImportError:
        return None
    try:
        return grp.getgrnam(name)[2]
    except KeyError:
        return None


def _get_uid(name):
    """User id for a user name, or None."""
    if name is None:
        return None
    try:
        import pwd
    except ImportError:
        return None
    try:
        return pwd.getpwnam(name)[2]
    except KeyError:
        return None


def chown(path, user=None, group=None, *, dir_fd=None, follow_symlinks=True):
    """Change the owner and/or group of path.

    user and group may be names or numeric ids; at least one is required.
    """
    sys.audit("shutil.chown", path, user, group)
    if user is None and group is None:
        raise ValueError("user and/or group must be set")

    uid = -1
    if user is not None:
        uid = user
        if isinstance(user, str):
            uid = _get_uid(user)
            if uid is None:
                raise LookupError("no such user: {!r}".format(user))

    gid = -1
    if group is not None:
        gid = group
        if not isinstance(group, int):
            gid = _get_gid(group)
            if gid is None:
                raise LookupError("no such group: {!r}".format(group))

    os.chown(path, uid, gid, dir_fd=dir_fd, follow_symlinks=follow_symlinks)


def _env_dimension(name):
    try:
        return int(os.environ[name])
    except (KeyError, ValueError):
        return 0


def get_terminal_size(fallback=(80, 24)):
    """Return the terminal size as os.terminal_size(columns, lines).

    The COLUMNS and LINES environment variables win when they hold
    positive integers; otherwise the terminal attached to
    sys.__stdout__ is queried, and *fallback* is used if that fails.
    """
    columns = _env_dimension("COLUMNS")
    lines = _env_dimension("LINES")
    if columns <= 0 or lines <= 0:
        try:
            measured = os.get_terminal_size(sys.__stdout__.fileno())
        except (AttributeError, ValueError, OSError):
            measured = os.terminal_size(fallback)
        if columns <= 0:
            columns = measured.columns or fallback[0]
        if lines <= 0:
            lines = measured.lines or fallback[1]
    return os.terminal_size((columns, lines))


# --------------------------------------------------------------------------
# which

def _access_check(fn, mode):
    return os.path.exists(fn) and os.access(fn, mode) and not os.path.isdir(fn)


def _win_path_needs_curdir(cmd, mode):
    """Windows: whether the current directory is implicitly searched."""
    return (not (mode & os.X_OK)) or _winapi.NeedCurrentDirectoryForExePath(cmd)


def which(cmd, mode=os.F_OK | os.X_OK, path=None):
    """Return the path of the executable *cmd* found on *path* (default:
    $PATH), or None.  *mode* is the os.access() mode to require."""
    use_bytes = isinstance(cmd, bytes)

    if os.path.dirname(cmd):
        # A path with a directory part is checked as is, never searched.
        return cmd if _access_check(cmd, mode) else None

    if path is None:
        path = os.environ.get("PATH", None)
        if path is None:
            try:
                path = os.confstr("CS_PATH")
            except (AttributeError, ValueError):
                path = os.defpath
    if not path:
        return None

    if use_bytes:
        dirs = os.fsencode(path).split(os.fsencode(os.pathsep))
    else:
        dirs = os.fsdecode(path).split(os.pathsep)

    if _WINDOWS:
        curdir = os.fsencode(os.curdir) if use_bytes else os.curdir
        if _win_path_needs_curdir(cmd, mode):
            dirs.insert(0, curdir)
        pathext_source = os.environ.get("PATHEXT", "") or ".COM;.EXE;.BAT;.CMD;.VBS;.JS;.WS;.MSC"
        exts = [ext for ext in pathext_source.split(os.pathsep) if ext]
        if use_bytes:
            exts = [os.fsencode(ext) for ext in exts]
        candidates = []
        lower = cmd.lower()
        if any(lower.endswith(ext.lower()) for ext in exts):
            candidates.append(cmd)
        if mode & os.X_OK:
            candidates.extend(cmd + ext for ext in exts)
        else:
            candidates.append(cmd)
        if not candidates or candidates[0] is not cmd:
            if cmd not in candidates:
                candidates.append(cmd)
    else:
        candidates = [cmd]

    seen = set()
    for directory in dirs:
        key = os.path.normcase(directory)
        if key in seen:
            continue
        seen.add(key)
        for name in candidates:
            full = os.path.join(directory, name)
            if _access_check(full, mode):
                return full
    return None


# --------------------------------------------------------------------------
# Archives

def _tar_owner_filter(owner, group):
    uid = _get_uid(owner)
    gid = _get_gid(group)

    def _apply(tarinfo):
        if gid is not None:
            tarinfo.gid = gid
            tarinfo.gname = group
        if uid is not None:
            tarinfo.uid = uid
            tarinfo.uname = owner
        return tarinfo
    return _apply


def _ensure_parent_dir(archive_name, dry_run, logger):
    parent = os.path.dirname(archive_name)
    if parent and not os.path.exists(parent):
        if logger is not None:
            logger.info("creating %s", parent)
        if not dry_run:
            os.makedirs(parent)


_TAR_COMPRESSORS = {
    None: ("", ""),
    "gzip": ("gz", ".gz"),
    "bzip2": ("bz2", ".bz2"),
    "xz": ("xz", ".xz"),
    "zst": ("zst", ".zst"),
}


def _make_tarball(base_name, base_dir, compress="gzip", verbose=0, dry_run=0,
                  owner=None, group=None, logger=None, root_dir=None):
    """Create a (possibly compressed) tar archive of base_dir; returns the
    archive file name (base_name + ".tar" + compression extension)."""
    available = {None, "gzip" if _ZLIB_SUPPORTED else None,
                 "bzip2" if _BZ2_SUPPORTED else None,
                 "xz" if _LZMA_SUPPORTED else None,
                 "zst" if _ZSTD_SUPPORTED else None}
    if compress not in _TAR_COMPRESSORS or (compress is not None
                                            and compress not in available):
        raise ValueError("bad value for 'compress', or compression format not "
                         "supported : {0}".format(compress))
    mode_suffix, extension = _TAR_COMPRESSORS[compress]

    if root_dir is not None:
        base_name = os.path.abspath(base_name)
    archive_name = base_name + ".tar" + extension
    _ensure_parent_dir(archive_name, dry_run, logger)
    if logger is not None:
        logger.info("Creating tar archive")

    if not dry_run:
        import tarfile
        arcname = base_dir
        source = base_dir if root_dir is None else os.path.join(root_dir, base_dir)
        with tarfile.open(archive_name, "w|" + mode_suffix) as tar:
            tar.add(source, arcname, filter=_tar_owner_filter(owner, group))
    return archive_name

_make_tarball.supports_root_dir = True


def _make_zipfile(base_name, base_dir, verbose=0, dry_run=0, logger=None,
                  owner=None, group=None, root_dir=None):
    """Create a ZIP archive of base_dir; returns base_name + ".zip"."""
    import zipfile

    if root_dir is not None:
        base_name = os.path.abspath(base_name)
    zip_name = base_name + ".zip"
    _ensure_parent_dir(zip_name, dry_run, logger)
    if logger is not None:
        logger.info("creating '%s' and adding '%s' to it", zip_name, base_dir)
    if dry_run:
        return zip_name

    own_file = os.path.abspath(zip_name)
    start = base_dir if root_dir is None else os.path.join(root_dir, base_dir)
    start = os.path.normpath(start)
    top_arcname = os.path.normpath(base_dir)

    def arc_dir(dirpath):
        rel = dirpath if root_dir is None else os.path.relpath(dirpath, root_dir)
        return os.path.normpath(rel)

    with zipfile.ZipFile(zip_name, "w", compression=zipfile.ZIP_DEFLATED) as zf:
        if top_arcname != os.curdir:
            zf.write(start, top_arcname)
            if logger is not None:
                logger.info("adding '%s'", top_arcname)
        for dirpath, dirnames, filenames in os.walk(start):
            prefix = arc_dir(dirpath)
            for name in sorted(dirnames):
                arcname = os.path.normpath(os.path.join(prefix, name))
                zf.write(os.path.join(dirpath, name), arcname)
                if logger is not None:
                    logger.info("adding '%s'", arcname)
            for name in filenames:
                full = os.path.normpath(os.path.join(dirpath, name))
                if not os.path.isfile(full) or os.path.abspath(full) == own_file:
                    continue
                arcname = os.path.normpath(os.path.join(prefix, name))
                zf.write(full, arcname)
                if logger is not None:
                    logger.info("adding '%s'", arcname)
    return zip_name

_make_zipfile.supports_root_dir = True


# name -> (function, extra keyword arguments, description)
_ARCHIVE_FORMATS = {
    "tar": (_make_tarball, [("compress", None)], "uncompressed tar file"),
}
if _ZLIB_SUPPORTED:
    _ARCHIVE_FORMATS["gztar"] = (_make_tarball, [("compress", "gzip")],
                                 "gzip'ed tar-file")
    _ARCHIVE_FORMATS["zip"] = (_make_zipfile, [], "ZIP file")
if _BZ2_SUPPORTED:
    _ARCHIVE_FORMATS["bztar"] = (_make_tarball, [("compress", "bzip2")],
                                 "bzip2'ed tar-file")
if _LZMA_SUPPORTED:
    _ARCHIVE_FORMATS["xztar"] = (_make_tarball, [("compress", "xz")],
                                 "xz'ed tar-file")
if _ZSTD_SUPPORTED:
    _ARCHIVE_FORMATS["zstdtar"] = (_make_tarball, [("compress", "zst")],
                                   "zstd'ed tar-file")


def get_archive_formats():
    """List of (name, description) pairs for the registered archivers."""
    return sorted((name, info[2]) for name, info in _ARCHIVE_FORMATS.items())


def _validate_extra_args(extra_args):
    if not isinstance(extra_args, (tuple, list)):
        raise TypeError("extra_args needs to be a sequence")
    for item in extra_args:
        if not isinstance(item, (tuple, list)) or len(item) != 2:
            raise TypeError("extra_args elements are : (arg_name, value)")


def register_archive_format(name, function, extra_args=None, description=""):
    """Register an archiver: function(base_name, base_dir, **kwargs).

    extra_args is a sequence of (name, value) pairs passed as keyword
    arguments.  Archivers with a true ``supports_root_dir`` attribute get
    root_dir as a keyword argument instead of a chdir().
    """
    if extra_args is None:
        extra_args = []
    if not callable(function):
        raise TypeError("The %s object is not callable" % function)
    _validate_extra_args(extra_args)
    _ARCHIVE_FORMATS[name] = (function, extra_args, description)


def unregister_archive_format(name):
    del _ARCHIVE_FORMATS[name]


def make_archive(base_name, format, root_dir=None, base_dir=None, verbose=0,
                 dry_run=0, owner=None, group=None, logger=None):
    """Create an archive file (zip, tar, ...) and return its name.

    base_name: archive path without the format extension.
    format: a name from get_archive_formats().
    root_dir: directory the archive is relative to (default: cwd).
    base_dir: directory inside root_dir to archive (default: everything).
    owner/group: ownership recorded in tar archives.
    """
    sys.audit("shutil.make_archive", base_name, format, root_dir, base_dir)
    try:
        func, extra, _description = _ARCHIVE_FORMATS[format]
    except KeyError:
        raise ValueError("unknown archive format '%s'" % format) from None

    kwargs = {"dry_run": dry_run, "logger": logger,
              "owner": owner, "group": group}
    kwargs.update(dict(extra))

    base_name = os.fspath(base_name)
    base_dir = os.curdir if base_dir is None else os.fspath(base_dir)

    previous_cwd = None
    if root_dir is not None:
        root_dir = os.fspath(root_dir)
        if not stat.S_ISDIR(os.stat(root_dir).st_mode):
            raise NotADirectoryError(errno.ENOTDIR, "Not a directory", root_dir)
        if getattr(func, "supports_root_dir", False):
            kwargs["root_dir"] = root_dir
        else:
            # Old-style archiver: run it from inside root_dir.
            previous_cwd = os.getcwd()
            base_name = os.path.abspath(base_name)
            if logger is not None:
                logger.debug("changing into '%s'", root_dir)
            if not dry_run:
                os.chdir(root_dir)
            else:
                previous_cwd = None

    try:
        return func(base_name, base_dir, **kwargs)
    finally:
        if previous_cwd is not None:
            if logger is not None:
                logger.debug("changing back to '%s'", previous_cwd)
            os.chdir(previous_cwd)


# -- unpacking

def get_unpack_formats():
    """List of (name, extensions, description) for the registered
    unpackers."""
    return sorted((name, info[0], info[3]) for name, info in _UNPACK_FORMATS.items())


def _check_unpack_options(extensions, function, extra_args):
    owners = {}
    for name, info in _UNPACK_FORMATS.items():
        for ext in info[0]:
            owners[ext] = name
    for ext in extensions:
        if ext in owners:
            raise RegistryError('%s is already registered for "%s"'
                                % (ext, owners[ext]))
    if not callable(function):
        raise TypeError("The registered function must be a callable")


def register_unpack_format(name, extensions, function, extra_args=None,
                           description=""):
    """Register an unpacker: function(filename, extract_dir, **kwargs).

    extensions lists the file name endings handled by the format.
    """
    if extra_args is None:
        extra_args = []
    _check_unpack_options(extensions, function, extra_args)
    _UNPACK_FORMATS[name] = (extensions, function, extra_args, description)


def unregister_unpack_format(name):
    del _UNPACK_FORMATS[name]


def _ensure_directory(path):
    parent = os.path.dirname(path)
    if parent and not os.path.isdir(parent):
        os.makedirs(parent)


def _zip_member_target(extract_dir, member_name):
    """Where a ZIP member should be extracted, or None for unsafe names
    (absolute paths or ones climbing out with '..')."""
    if member_name.startswith("/"):
        return None
    parts = member_name.split("/")
    if ".." in parts:
        return None
    if _WINDOWS:
        if any(":" in part or "\\" in part for part in parts):
            return None
    return os.path.join(extract_dir, *parts)


def _unpack_zipfile(filename, extract_dir):
    """Extract a ZIP archive, skipping members with unsafe names."""
    import zipfile
    if not zipfile.is_zipfile(filename):
        raise ReadError("%s is not a zip file" % filename)
    with zipfile.ZipFile(filename) as archive:
        for info in archive.infolist():
            target = _zip_member_target(extract_dir, info.filename)
            if not target:
                continue
            _ensure_directory(target)
            if info.filename.endswith("/"):
                continue
            with archive.open(info.filename) as source, open(target, "wb") as sink:
                copyfileobj(source, sink)


def _unpack_tarfile(filename, extract_dir, *, filter=None):
    """Extract a tar archive (any compression tarfile understands)."""
    import tarfile
    try:
        archive = tarfile.open(filename)
    except tarfile.TarError:
        raise ReadError("%s is not a compressed or uncompressed tar file"
                        % filename)
    try:
        archive.extractall(extract_dir, filter=filter)
    finally:
        archive.close()


# name -> (extensions, function, extra keyword arguments, description)
_UNPACK_FORMATS = {
    "tar": ([".tar"], _unpack_tarfile, [], "uncompressed tar file"),
    "zip": ([".zip"], _unpack_zipfile, [], "ZIP file"),
}
if _ZLIB_SUPPORTED:
    _UNPACK_FORMATS["gztar"] = ([".tar.gz", ".tgz"], _unpack_tarfile, [],
                                "gzip'ed tar-file")
if _BZ2_SUPPORTED:
    _UNPACK_FORMATS["bztar"] = ([".tar.bz2", ".tbz2"], _unpack_tarfile, [],
                                "bzip2'ed tar-file")
if _LZMA_SUPPORTED:
    _UNPACK_FORMATS["xztar"] = ([".tar.xz", ".txz"], _unpack_tarfile, [],
                                "xz'ed tar-file")
if _ZSTD_SUPPORTED:
    _UNPACK_FORMATS["zstdtar"] = ([".tar.zst", ".tzst"], _unpack_tarfile, [],
                                  "zstd'ed tar-file")


def _find_unpack_format(filename):
    for name, info in _UNPACK_FORMATS.items():
        if any(filename.endswith(ext) for ext in info[0]):
            return name
    return None


def unpack_archive(filename, extract_dir=None, format=None, *, filter=None):
    """Unpack an archive into extract_dir (default: the current directory).

    format is a registered unpack format name; when omitted it is guessed
    from the file name.  filter is passed to tar extraction (see
    tarfile's extraction filters).
    """
    sys.audit("shutil.unpack_archive", filename, extract_dir, format)
    if extract_dir is None:
        extract_dir = os.getcwd()
    extract_dir = os.fspath(extract_dir)
    filename = os.fspath(filename)

    extra_kwargs = {} if filter is None else {"filter": filter}

    if format is None:
        format = _find_unpack_format(filename)
        if format is None:
            raise ReadError("Unknown archive format '{0}'".format(filename))
        _extensions, func, extra, _description = _UNPACK_FORMATS[format]
    else:
        try:
            _extensions, func, extra, _description = _UNPACK_FORMATS[format]
        except KeyError:
            raise ValueError("Unknown unpack format '{0}'".format(format)) from None

    kwargs = dict(extra)
    kwargs.update(extra_kwargs)
    func(filename, extract_dir, **kwargs)
